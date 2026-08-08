//! Minion supervisor — top-level process manager (roadmap 1-2-5).
//!
//! Owns: PID file locking, signal handling (SIGTERM/SIGINT), DB health
//! check loop, audit event emission, and delegates to
//! [`ChildWorkerSupervisor`](super::child_supervisor::ChildWorkerSupervisor)
//! for the spawn/respawn loop.
//!
//! Mirrors TS `MinionSupervisor` (`supervisor.ts`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use tracing::{error, info, warn};

use zbrain_core::engine::BrainEngine;

use crate::child_supervisor::{
    ChildSupervisorEvent, ChildSupervisorOpts, ChildWorkerSupervisor,
};

// ─── Types ───────────────────────────────────────────────────────────────────

/// Lifecycle events emitted by the supervisor.
/// Mirrors TS `SupervisorEvent` + `SupervisorEmission`.
#[derive(Debug, Clone)]
pub enum SupervisorEvent {
    Started,
    WorkerSpawned { pid: u32 },
    WorkerExited { code: Option<i32>, crash_count: u32 },
    WorkerSpawnFailed { error: String },
    Backoff { ms: u64, reason: String },
    HealthWarn { reason: String },
    HealthError { error: String },
    MaxCrashesExceeded { count: u32, max: u32 },
    ShuttingDown,
    Stopped,
}

/// Supervisor configuration.
/// Mirrors TS `SupervisorOpts`.
#[derive(Debug, Clone)]
pub struct SupervisorOpts {
    /// Worker concurrency (passed to child as `--concurrency`).
    pub concurrency: u32,
    /// Queue name (passed to child as `--queue`).
    pub queue: String,
    /// PID file path. Default: `~/.zbrain/supervisor.pid`.
    pub pid_file: PathBuf,
    /// Max consecutive crashes before giving up. Default: 10.
    pub max_crashes: u32,
    /// DB health check interval in ms. 0 = disabled. Default: 60_000.
    pub health_interval_ms: u64,
    /// Path to the zbrain CLI binary to spawn as child.
    pub cli_path: String,
    /// Whether to allow shell jobs. Default: false.
    pub allow_shell_jobs: bool,
    /// Max RSS in MB for the child. Default: 2048.
    pub max_rss_mb: u64,
}

impl Default for SupervisorOpts {
    fn default() -> Self {
        Self {
            concurrency: 2,
            queue: "default".to_string(),
            pid_file: zbrain_core::paths::zbrain_home()
                .unwrap_or_else(|| PathBuf::from(".zbrain"))
                .join("supervisor.pid"),
            max_crashes: 10,
            health_interval_ms: 60_000,
            cli_path: String::new(),
            allow_shell_jobs: false,
            max_rss_mb: 2048,
        }
    }
}

// ─── Exit codes ──────────────────────────────────────────────────────────────

/// Exit codes used by the supervisor process itself.
pub mod exit_codes {
    pub const CLEAN: i32 = 0;
    pub const MAX_CRASHES: i32 = 1;
    pub const LOCK_HELD: i32 = 2;
    pub const PID_UNWRITABLE: i32 = 3;
}

// ─── PID lock ────────────────────────────────────────────────────────────────

/// Result of attempting to acquire the PID file lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PidLockResult {
    /// Lock acquired successfully.
    Acquired,
    /// Another supervisor holds the lock (PID file exists + process alive).
    Held,
    /// PID file directory/path is not writable.
    Unwritable,
}

/// Try to acquire the PID file lock atomically (`O_CREAT | O_EXCL`).
/// On EEXIST, check if the PID in the file is still alive (Unix `kill(pid, 0)`)
/// and remove stale locks.
#[allow(unused_variables)]
pub fn acquire_pid_lock(pid_file: &std::path::Path) -> PidLockResult {
    let pid = std::process::id();

    // Ensure parent directory exists.
    if let Some(parent) = pid_file.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!(path = %parent.display(), error = %e, "PID lock: cannot create parent dir");
            return PidLockResult::Unwritable;
        }
    }

    // First attempt: atomic create.
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(pid_file)
    {
        Ok(mut f) => {
            use std::io::Write;
            let _ = writeln!(f, "{pid}");
            return PidLockResult::Acquired;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // PID file exists — check if the PID is still alive.
            match std::fs::read_to_string(pid_file) {
                Ok(content) => {
                    let existing_pid: i32 = match content.trim().parse() {
                        Ok(p) => p,
                        Err(_) => {
                            // Corrupt PID file — remove and retry.
                            let _ = std::fs::remove_file(pid_file);
                            return acquire_pid_lock(pid_file);
                        }
                    };

                    if is_pid_alive(existing_pid) {
                        return PidLockResult::Held;
                    }

                    // Stale lock — remove and retry once.
                    let _ = std::fs::remove_file(pid_file);
                    match std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(pid_file)
                    {
                        Ok(mut f) => {
                            use std::io::Write;
                            let _ = writeln!(f, "{pid}");
                            PidLockResult::Acquired
                        }
                        Err(_) => PidLockResult::Held, // race: someone else grabbed it
                    }
                }
                Err(_) => PidLockResult::Unwritable,
            }
        }
        Err(_) => PidLockResult::Unwritable,
    }
}

/// Release the PID file lock by deleting the file.
pub fn release_pid_lock(pid_file: &std::path::Path) {
    let _ = std::fs::remove_file(pid_file);
}

// ─── PID liveness check ──────────────────────────────────────────────────────

/// Check if a process with the given PID is still alive.
/// On Unix: `kill(pid, 0)`. On Windows: best-effort (always returns false).
fn is_pid_alive(pid: i32) -> bool {
    #[cfg(unix)]
    {
        // `/proc/<pid>` exists iff the process is alive. Equivalent in intent to
        // `kill(pid, 0)` but stays within safe std — the workspace forbids `unsafe`
        // (`unsafe_code = "forbid"`) and this crate has no `libc` dependency.
        // Mirrors `schema_pack::pack_lock::default_is_pid_alive`.
        pid > 0 && std::path::Path::new(&format!("/proc/{}", pid)).exists()
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false // Not implemented on this platform.
    }
}

// ─── MinionSupervisor ────────────────────────────────────────────────────────

/// Top-level process manager. Acquires a PID lock, handles system signals,
/// runs periodic DB health checks, and delegates child spawn/respawn to
/// [`ChildWorkerSupervisor`].
///
/// Mirrors TS `MinionSupervisor` class.
pub struct MinionSupervisor {
    opts: SupervisorOpts,
    engine: Arc<dyn BrainEngine>,
    event_tx: mpsc::Sender<SupervisorEvent>,
    lock_acquired: bool,
}

impl MinionSupervisor {
    /// Create a new supervisor. Does NOT acquire the PID lock or start
    /// signal handlers — call `start()` for that.
    pub fn new(
        opts: SupervisorOpts,
        engine: Arc<dyn BrainEngine>,
        event_tx: mpsc::Sender<SupervisorEvent>,
    ) -> Self {
        Self {
            opts,
            engine,
            event_tx,
            lock_acquired: false,
        }
    }

    /// Access the event sender (for wiring additional consumers).
    pub fn event_sender(&self) -> mpsc::Sender<SupervisorEvent> {
        self.event_tx.clone()
    }

    /// Start the supervisor: acquire PID lock, register signal handlers,
    /// start health check timer, run the child spawn loop.
    /// Blocks until shutdown or max crashes exceeded.
    /// Returns the exit code.
    pub async fn start(&mut self) -> i32 {
        let _ = self.event_tx.send(SupervisorEvent::Started).await;

        // Step 1: PID lock
        match acquire_pid_lock(&self.opts.pid_file) {
            PidLockResult::Acquired => {
                self.lock_acquired = true;
            }
            PidLockResult::Held => {
                error!("PID lock held by another supervisor");
                return exit_codes::LOCK_HELD;
            }
            PidLockResult::Unwritable => {
                error!("PID file not writable: {}", self.opts.pid_file.display());
                return exit_codes::PID_UNWRITABLE;
            }
        }

        // Step 2: Register PID lock cleanup on process exit.
        let pid_file = self.opts.pid_file.clone();
        // We use a shutdown hook approach — release on drop.

        // Step 3: Build child supervisor.
        let (child_tx, mut child_rx) = mpsc::channel::<ChildSupervisorEvent>(32);
        let is_stopping = Arc::new(AtomicBool::new(false));
        let stopping = is_stopping.clone();

        let child_args = vec![
            "jobs".to_string(),
            "work".to_string(),
            "--concurrency".to_string(),
            self.opts.concurrency.to_string(),
            "--queue".to_string(),
            self.opts.queue.clone(),
            "--max-rss".to_string(),
            self.opts.max_rss_mb.to_string(),
        ];

        let mut child_env = HashMap::new();
        child_env.insert("ZBRAIN_SUPERVISED".to_string(), "1".to_string());
        if self.opts.allow_shell_jobs {
            child_env.insert("ZBRAIN_ALLOW_SHELL_JOBS".to_string(), "1".to_string());
        }

        let child_opts = ChildSupervisorOpts {
            cli_path: self.opts.cli_path.clone(),
            args: child_args,
            env: child_env,
            max_crashes: self.opts.max_crashes,
            ..Default::default()
        };

        let on_max_crashes = {
            let tx = self.event_tx.clone();
            Arc::new(move |count, max| {
                let _ = tx.try_send(SupervisorEvent::MaxCrashesExceeded { count, max });
            })
        };

        let mut child_sup =
            ChildWorkerSupervisor::new(child_opts, child_tx, on_max_crashes, stopping.clone());

        // Step 4: Spawn health check timer (if enabled).
        let health_interval = self.opts.health_interval_ms;
        let engine = self.engine.clone();
        let health_tx = self.event_tx.clone();
        let health_stopping = stopping.clone();

        if health_interval > 0 {
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_millis(health_interval));
                interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
                let mut consecutive_failures = 0u32;

                loop {
                    if health_stopping.load(Ordering::Acquire) {
                        break;
                    }
                    interval.tick().await;

                    match engine.health_check().await {
                        Ok(health) => {
                            consecutive_failures = 0;
                            if health.stalled_count > 10 {
                                let _ = health_tx.try_send(SupervisorEvent::HealthWarn {
                                    reason: format!(
                                        "stalled_jobs: {} stalled",
                                        health.stalled_count
                                    ),
                                });
                            }
                            if health.waiting_count > 0
                                && health.last_completed_at.is_none()
                            {
                                let _ = health_tx.try_send(SupervisorEvent::HealthWarn {
                                    reason: "no_recent_completions".to_string(),
                                });
                            }
                        }
                        Err(e) => {
                            consecutive_failures += 1;
                            warn!(
                                consecutive_failures,
                                error = %e,
                                "Supervisor health check failed"
                            );
                            if consecutive_failures >= 3 {
                                let _ = health_tx.try_send(SupervisorEvent::HealthError {
                                    error: format!(
                                        "db_connection_degraded: {} consecutive failures",
                                        consecutive_failures
                                    ),
                                });
                            }
                        }
                    }
                }
            });
        }

        // Step 5: Spawn task to relay child events → supervisor events.
        let relay_tx = self.event_tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = child_rx.recv().await {
                let sup_ev = match ev {
                    ChildSupervisorEvent::WorkerSpawned { pid, .. } => {
                        SupervisorEvent::WorkerSpawned { pid }
                    }
                    ChildSupervisorEvent::WorkerExited { code, crash_count, .. } => {
                        SupervisorEvent::WorkerExited { code, crash_count }
                    }
                    ChildSupervisorEvent::WorkerSpawnFailed { error } => {
                        SupervisorEvent::WorkerSpawnFailed { error }
                    }
                    ChildSupervisorEvent::Backoff { ms, reason, .. } => {
                        SupervisorEvent::Backoff { ms, reason: format!("{reason:?}") }
                    }
                    ChildSupervisorEvent::HealthWarn { reason } => {
                        SupervisorEvent::HealthWarn { reason }
                    }
                };
                let _ = relay_tx.send(sup_ev).await;
            }
        });

        // Step 6: Set up signal handlers (Unix only).
        #[cfg(unix)]
        {
            let stopping_sig = stopping.clone();
            let tx_sig = self.event_tx.clone();
            tokio::spawn(async move {
                let mut sigterm = match tokio::signal::unix::signal(
                    tokio::signal::unix::SignalKind::terminate(),
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        error!(error = %e, "Failed to register SIGTERM handler");
                        return;
                    }
                };
                let mut sigint = match tokio::signal::unix::signal(
                    tokio::signal::unix::SignalKind::interrupt(),
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        error!(error = %e, "Failed to register SIGINT handler");
                        return;
                    }
                };

                tokio::select! {
                    _ = sigterm.recv() => {}
                    _ = sigint.recv() => {}
                }
                info!("Supervisor received shutdown signal");
                let _ = tx_sig.send(SupervisorEvent::ShuttingDown).await;
                stopping_sig.store(true, Ordering::Release);
            });
        }

        // Step 7: Run the child spawn/respawn loop.
        child_sup.run().await;

        let _ = self.event_tx.send(SupervisorEvent::Stopped).await;

        // Clean up PID lock.
        if self.lock_acquired {
            release_pid_lock(&pid_file);
        }

        if child_sup.crash_count() >= self.opts.max_crashes {
            exit_codes::MAX_CRASHES
        } else {
            exit_codes::CLEAN
        }
    }
}

impl Drop for MinionSupervisor {
    fn drop(&mut self) {
        if self.lock_acquired {
            release_pid_lock(&self.opts.pid_file);
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_lock_creates_pid_file() {
        let tmp = std::env::temp_dir().join("zbrain_test_supervisor.pid");
        let _ = std::fs::remove_file(&tmp);

        let result = acquire_pid_lock(&tmp);
        assert_eq!(result, PidLockResult::Acquired);

        let content = std::fs::read_to_string(&tmp).unwrap();
        let pid: i32 = content.trim().parse().unwrap();
        assert!(pid > 0);

        release_pid_lock(&tmp);
        assert!(!tmp.exists());
    }

    #[test]
    fn acquire_lock_detects_existing_lock() {
        let tmp = std::env::temp_dir().join("zbrain_test_supervisor_existing.pid");
        let _ = std::fs::remove_file(&tmp);

        assert_eq!(acquire_pid_lock(&tmp), PidLockResult::Acquired);

        let result = acquire_pid_lock(&tmp);
        #[cfg(unix)]
        assert_eq!(result, PidLockResult::Held);
        #[cfg(not(unix))]
        assert_eq!(result, PidLockResult::Acquired); // no kill(pid,0) → stale cleanup kicks in

        release_pid_lock(&tmp);
    }

    #[test]
    fn acquire_lock_cleans_stale_pid() {
        let tmp = std::env::temp_dir().join("zbrain_test_stale.pid");
        let _ = std::fs::remove_file(&tmp);

        std::fs::write(&tmp, "99999999\n").unwrap();

        let result = acquire_pid_lock(&tmp);
        assert_eq!(result, PidLockResult::Acquired);

        release_pid_lock(&tmp);
    }

    #[test]
    fn default_opts_has_sensible_values() {
        let opts = SupervisorOpts::default();
        assert_eq!(opts.concurrency, 2);
        assert_eq!(opts.queue, "default");
        assert_eq!(opts.max_crashes, 10);
        assert_eq!(opts.health_interval_ms, 60_000);
        assert_eq!(opts.max_rss_mb, 2048);
        assert!(!opts.allow_shell_jobs);
    }
}
