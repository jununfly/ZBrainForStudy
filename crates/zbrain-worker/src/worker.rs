//! `MinionWorker` — the queue consume core (roadmap 1-2-2).
//!
//! A worker holds an engine handle and a registry of
//! [`MinionHandler`](zbrain_core::minions::MinionHandler)s keyed by job name.
//! Its heart is [`MinionWorker::process_next_job`]: one turn of the consume
//! loop that promotes due delayed jobs, claims (at most) one waiting job whose
//! name it can handle, dispatches it to the handler, and drives the resulting
//! complete / fail-with-retry transition. [`MinionWorker::run`] is a thin
//! serial loop around it (concurrency = 1 for this slice).
//!
//! ## Deep-module boundary
//!
//! `process_next_job` is the deep module: callers get a single small method
//! whose return value ([`ProcessOutcome`]) says only "did work" vs "idle",
//! while all the claim/dispatch/complete/fail orchestration is hidden. Tests
//! drive it directly (no timers, no loop), which is why the loop concerns
//! (poll sleep, running flag, later concurrency + signals) stay out of it.
//!
//! ## TS reference
//!
//! - consume loop — `src/core/minions/worker.ts` L430-472
//! - `executeJob` (dispatch + complete/fail) — `worker.ts` L673-847

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use zbrain_core::engine::BrainEngine;
use zbrain_core::minions::types::{FailOutcome, MinionJob};
use zbrain_core::minions::{MinionHandler, MinionJobContext, MinionQueue, MinionWorkerOpts};
use zbrain_core::Result;

use crate::backoff::{calculate_backoff, BackoffInput};

/// Sentinel kind marking a handler error as non-retryable: the worker routes it
/// straight to `dead` instead of scheduling a backoff retry. Mirrors the TS
/// `UnrecoverableError` class check (`worker.ts` L814). We express it as a
/// `StructuredError` kind (roadmap 1-2-2 decision 1) so the 1-2-1 handler
/// contract — `handle(...) -> Result<Value>` — stays unchanged.
pub const UNRECOVERABLE_KIND: &str = "unrecoverable";

/// Build a non-retryable handler error. A handler returning this fails the job
/// terminally (`dead`) on the first attempt, bypassing the retry/backoff curve.
#[must_use]
pub fn unrecoverable(message: impl Into<String>) -> zbrain_core::error::StructuredError {
    zbrain_core::error::StructuredError::new(UNRECOVERABLE_KIND, "unrecoverable", message.into())
}

/// Outcome of one [`MinionWorker::process_next_job`] turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessOutcome {
    /// A job was claimed and driven to a terminal-or-retry transition. Carries
    /// the job id that was processed.
    Processed(i64),
    /// No eligible waiting job for this worker's registered names.
    Idle,
}

/// Reason a self-health-check deemed the worker unhealthy.  Mirrors TS
/// `UnhealthyReason` (`worker.ts` L93-95).  Emitted via the unhealthy channel
/// (set with [`MinionWorker::on_unhealthy`]).
#[derive(Debug, Clone)]
pub enum UnhealthyReason {
    /// DB probe failed `db_fail_exit_after` consecutive times.
    DbDead {
        consecutive_failures: u32,
        message: String,
    },
    /// Worker hasn't completed any jobs in `stall_exit_after_ms` while
    /// registered-handler jobs sat in `waiting`.
    Stalled {
        waiting_count: u64,
        idle_minutes: u64,
    },
}

/// A backend-native job worker. Serial in this slice (concurrency = 1).
pub struct MinionWorker {
    engine: Arc<dyn BrainEngine>,
    handlers: HashMap<String, Arc<dyn MinionHandler>>,
    opts: MinionWorkerOpts,
    worker_id: String,
    /// Fires on process shutdown (SIGTERM/SIGINT). Handed to every job context
    /// as its `shutdown` token.
    shutdown: CancellationToken,
    /// Incremented after every successful job completion.  Read by the health-
    /// check stall detector.
    jobs_completed: Arc<AtomicU64>,
    /// Sender for unhealthy events.  Callers set it via `on_unhealthy()`.
    unhealthy_tx: Option<mpsc::UnboundedSender<UnhealthyReason>>,
    /// Injectable RSS reader (bytes).  Default: calls `get_accurate_rss` from
    /// `crate::rss`.  Injectable for tests.
    get_rss: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl MinionWorker {
    /// Create a worker over an engine with the given options.
    #[must_use]
    pub fn new(engine: Arc<dyn BrainEngine>, opts: MinionWorkerOpts) -> Self {
        Self {
            engine,
            handlers: HashMap::new(),
            opts,
            worker_id: uuid_like(),
            shutdown: CancellationToken::new(),
            jobs_completed: Arc::new(AtomicU64::new(0)),
            unhealthy_tx: None,
            get_rss: Arc::new(|| crate::rss::get_accurate_rss(|| {
                std::fs::read_to_string("/proc/self/status")
            })),
        }
    }

    /// Register a handler for a job name. Mirrors TS `worker.register(name, fn)`.
    /// Returns `&mut Self` for chaining.
    pub fn register(&mut self, name: impl Into<String>, handler: Arc<dyn MinionHandler>) -> &mut Self {
        self.handlers.insert(name.into(), handler);
        self
    }

    /// Job names this worker can claim (registry keys). Empty until a handler
    /// is registered — [`process_next_job`](Self::process_next_job) claims
    /// nothing in that state (matches TS early return).
    #[must_use]
    pub fn registered_names(&self) -> Vec<String> {
        self.handlers.keys().cloned().collect()
    }

    /// Return a clone of the worker's shutdown token. Cancelling it causes
    /// [`run`](Self::run) to stop claiming new jobs and drain in-flight ones
    /// (30s timeout). Mirrors TS `shutdownAbort` (`worker.ts` L135).
    #[must_use]
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// Register an unhealthy-event channel.  The worker will send
    /// [`UnhealthyReason`] messages when the health check trips (DB dead or
    /// stalled).  Call this before [`run`](Self::run).  Mirrors TS
    /// `worker.on('unhealthy', ...)`.
    pub fn on_unhealthy(&mut self, tx: mpsc::UnboundedSender<UnhealthyReason>) -> &mut Self {
        self.unhealthy_tx = Some(tx);
        self
    }

    /// Replace the RSS reader (for tests).  Default calls `/proc/self/status`.
    pub fn set_rss_reader(&mut self, f: impl Fn() -> u64 + Send + Sync + 'static) -> &mut Self {
        self.get_rss = Arc::new(f);
        self
    }

    /// Check current RSS against `max_rss_mb`.  When exceeded, cancels the
    /// shutdown token (triggering graceful drain in run()).  Idempotent.
    /// Mirrors TS `checkMemoryLimit` (`worker.ts` L567-588).
    ///
    /// Returns `true` when shutdown was triggered.
    pub fn check_memory_limit(&self) -> bool {
        let Some(max_mb) = self.opts.max_rss_mb else {
            return false;
        };
        if max_mb == 0 {
            return false;
        }
        if self.shutdown.is_cancelled() {
            return false;
        }

        let rss = (self.get_rss)();
        let rss_mb = rss / (1024 * 1024);
        if rss_mb < max_mb {
            return false;
        }

        tracing::warn!(
            rss_mb = rss_mb,
            threshold_mb = max_mb,
            jobs_completed = self.jobs_completed.load(Ordering::Relaxed),
            "RSS watchdog: threshold exceeded, initiating graceful shutdown"
        );
        self.shutdown.cancel();
        true
    }

    /// One turn of the consume loop: promote due delayed jobs, then claim +
    /// dispatch at most one job. Returns whether work was done. This is the
    /// unit tests drive directly.
    pub async fn process_next_job(&self) -> Result<ProcessOutcome> {
        let queue = MinionQueue::new(&*self.engine);

        // Promote delayed jobs whose delay_until has passed. Errors here are
        // logged and swallowed in the TS loop (L433-437) so a promotion hiccup
        // never stalls claiming; we surface them as Err for the caller to log,
        // but the serial run() loop treats them as non-fatal.
        queue.promote_delayed().await?;

        let names = self.registered_names();
        let lock_token = format!("{}:{}", self.worker_id, now_millis());
        let Some(job) = queue
            .claim(&lock_token, self.opts.lock_duration_ms, &self.opts.queue, &names)
            .await?
        else {
            return Ok(ProcessOutcome::Idle);
        };

        let id = job.id;
        self.execute_job(job, lock_token).await?;
        Ok(ProcessOutcome::Processed(id))
    }

    /// Concurrent run loop: promotes delayed jobs, claims up to `concurrency`
    /// jobs, dispatches each as an independent task with its own lock-renewal
    /// timer and per-job timeout, and drains in-flight jobs on shutdown.
    ///
    /// Returns when the shutdown token is cancelled. After the loop exits,
    /// in-flight jobs are awaited with a 30s drain timeout (TS L480-488).
    /// Mirrors TS `start()` (`worker.ts` L430-503).
    pub async fn run(&self) -> Result<()> {
        let mut join_set: JoinSet<Result<()>> = JoinSet::new();
        let jobs_done = Arc::clone(&self.jobs_completed);

        // --- RSS watchdog (TS L270-275): periodic when max_rss_mb > 0 -------
        let _rss_task = if self.opts.max_rss_mb.unwrap_or(0) > 0 {
            let shutdown = self.shutdown.clone();
            let interval = Duration::from_millis(self.opts.rss_check_interval_ms as u64);
            let get_rss = Arc::clone(&self.get_rss);
            let max_mb = self.opts.max_rss_mb;
            Some(tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        _ = tokio::time::sleep(interval) => {}
                    }
                    let rss = get_rss();
                    let rss_mb = rss / (1024 * 1024);
                    if rss_mb >= max_mb.unwrap_or(u64::MAX) {
                        tracing::warn!(
                            rss_mb, threshold_mb = max_mb,
                            "RSS watchdog (periodic): exceeding limit, shutting down"
                        );
                        shutdown.cancel();
                        break;
                    }
                }
            }))
        } else {
            None
        };

        // --- Health check (TS L293-423): DB probe + stall detection ---------
        let _health_task = if self.opts.health_check_interval_ms > 0 {
            let engine = Arc::clone(&self.engine);
            let shutdown = self.shutdown.clone();
            let interval = Duration::from_millis(self.opts.health_check_interval_ms as u64);
            let db_fail_limit = self.opts.db_fail_exit_after;
            let stall_warn = Duration::from_millis(self.opts.stall_warn_after_ms as u64);
            let stall_exit = Duration::from_millis(self.opts.stall_exit_after_ms as u64);
            let probe_timeout = Duration::from_millis(self.opts.db_probe_timeout_ms as u64);
            let unhealthy_tx = self.unhealthy_tx.clone();
            let jobs_done = Arc::clone(&self.jobs_completed);
            let queue_name = self.opts.queue.clone();
            Some(tokio::spawn(async move {
                let mut consecutive_failures: u32 = 0;
                let mut last_completed: u64 = jobs_done.load(Ordering::Relaxed);
                let mut last_completion_time = tokio::time::Instant::now();
                loop {
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        _ = tokio::time::sleep(interval) => {}
                    }

                    // 1. DB liveness probe.
                    let probe_ok = tokio::time::timeout(probe_timeout, async {
                        // Use get_job(0) as a lightweight DB probe.
                        engine.get_job(0).await.map(|_| ())
                    })
                    .await;

                    match probe_ok {
                        Ok(Ok(())) => {
                            consecutive_failures = 0;
                        }
                        _ => {
                            consecutive_failures += 1;
                            if consecutive_failures >= db_fail_limit {
                                let reason = UnhealthyReason::DbDead {
                                    consecutive_failures,
                                    message: format!(
                                        "DB probe failed {} consecutive times",
                                        consecutive_failures
                                    ),
                                };
                                if let Some(ref tx) = unhealthy_tx {
                                    let _ = tx.send(reason);
                                }
                                break;
                            }
                            continue; // Skip stall check when DB is flaky.
                        }
                    }

                    // 2. Stall detection.
                    let current = jobs_done.load(Ordering::Relaxed);
                    if current > last_completed {
                        last_completed = current;
                        last_completion_time = tokio::time::Instant::now();
                    }

                    let idle = last_completion_time.elapsed();
                    if idle > stall_warn {
                        // Check if there are waiting jobs in this queue.
                        let filter = zbrain_core::minions::types::JobFilters {
                            status: Some(zbrain_core::minions::types::MinionJobStatus::Waiting),
                            queue: Some(queue_name.clone()),
                            name: None,
                            limit: None,
                            offset: None,
                        };
                        let waiting_count = engine
                            .get_jobs(&filter)
                            .await
                            .map(|jobs| jobs.len() as u64)
                            .unwrap_or(0);
                        if waiting_count > 0 && idle > stall_exit {
                            let reason = UnhealthyReason::Stalled {
                                waiting_count,
                                idle_minutes: idle.as_secs() / 60,
                            };
                            if let Some(ref tx) = unhealthy_tx {
                                let _ = tx.send(reason);
                            }
                            break;
                        }
                    }
                }
            }))
        } else {
            None
        };

        // --- Main consume loop (1-2-3) -------------------------------------
        while !self.shutdown.is_cancelled() {
            let queue = MinionQueue::new(&*self.engine);

            // Promote delayed jobs whose delay_until has passed.
            let _ = queue.promote_delayed().await;

            // Claim if under concurrency limit.
            if (join_set.len() as u32) < self.opts.concurrency {
                let lock_token = format!("{}:{}", self.worker_id, now_millis());
                let names = self.registered_names();
                if let Some(job) = queue
                    .claim(
                        &lock_token,
                        self.opts.lock_duration_ms,
                        &self.opts.queue,
                        &names,
                    )
                    .await?
                {
                    // Spawn dispatch as an independent task.
                    let engine = Arc::clone(&self.engine);
                    let handler = self.handlers.get(&job.name).cloned();
                    let signal = CancellationToken::new();
                    let shutdown = self.shutdown.clone();
                    let opts = self.opts.clone();
                    let jd = Arc::clone(&jobs_done);
                    join_set.spawn(async move {
                        Self::run_one_job(
                            engine,
                            handler,
                            job,
                            lock_token,
                            signal,
                            shutdown,
                            opts,
                            jd,
                        )
                        .await
                    });
                    continue; // try to fill remaining slots immediately
                } else if join_set.is_empty() {
                    // Idle: no jobs and nothing in flight.
                    tokio::time::sleep(Duration::from_millis(self.opts.poll_interval_ms as u64)).await;
                } else {
                    // Jobs running but none available — brief pause.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            } else {
                // At capacity — brief pause before re-checking for free slots.
                tokio::time::sleep(Duration::from_millis(100)).await;
            }

            // Reap completed tasks (non-blocking).
            while join_set.try_join_next().is_some() {}
        }

        // Graceful shutdown: drain in-flight jobs with 30s timeout (TS L480-488).
        let drain = async {
            while let Some(res) = join_set.join_next().await {
                let _ = res;
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(30), drain).await;

        Ok(())
    }

    /// Run one claimed job to completion in a spawned task context. Owns all
    /// data it needs (no `&self` borrow) so it can be `JoinSet::spawn`'d.
    ///
    /// Adds the 1-2-3 lifecycle concerns around the 1-2-2 dispatch core:
    /// - Per-job lock renewal at `lock_duration / 2` interval (cancels signal
    ///   on failure, TS L618-625).
    /// - Per-job wall-clock timeout (cancels signal after `timeout_ms`, TS
    ///   L634-658).
    /// - Signal / shutdown separation: timeout + lock-loss fire the per-job
    ///   `signal`; SIGTERM fires the global `shutdown` (already in context).
    async fn run_one_job(
        engine: Arc<dyn BrainEngine>,
        handler: Option<Arc<dyn MinionHandler>>,
        job: MinionJob,
        lock_token: String,
        signal: CancellationToken,
        shutdown: CancellationToken,
        opts: MinionWorkerOpts,
        jobs_completed: Arc<AtomicU64>,
    ) -> Result<()> {
        let queue = MinionQueue::new(&*engine);

        // Missing handler -> dead-letter (belt-and-suspenders; claim only
        // returns registered names).
        let Some(handler) = handler else {
            queue
                .fail_job(
                    job.id,
                    &lock_token,
                    &format!("No handler for job type '{}'", job.name),
                    FailOutcome::Dead,
                    0,
                )
                .await?;
            return Ok(());
        };

        let ctx = MinionJobContext::new(
            Arc::clone(&engine),
            job.id,
            job.name.clone(),
            job.data.clone(),
            job.attempts_made,
            lock_token.clone(),
            signal.clone(),
            shutdown,
        );

        // Spawn lock renewal (cancels signal on lease loss). TS L618-625.
        let renew_handle = {
            let engine = Arc::clone(&engine);
            let token = lock_token.clone();
            let sig = signal.clone();
            let dur = Duration::from_millis((opts.lock_duration_ms / 2).max(1) as u64);
            let lock_dur = opts.lock_duration_ms;
            let job_id = job.id;
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(dur);
                ticker.tick().await; // skip first immediate tick
                loop {
                    ticker.tick().await;
                    match engine.renew_job_lock(job_id, &token, lock_dur).await {
                        Ok(true) => continue,
                        _ => {
                            sig.cancel();
                            break;
                        }
                    }
                }
            })
        };

        // Run handler with concurrent timeout + signal cancellation.
        let result = if let Some(timeout_ms) = job.timeout_ms.filter(|&ms| ms > 0) {
            tokio::select! {
                r = handler.handle(&ctx) => r,
                _ = signal.cancelled() => {
                    Err(aborted_error("signal"))
                }
                _ = tokio::time::sleep(Duration::from_millis(timeout_ms as u64)) => {
                    signal.cancel();
                    Err(aborted_error("timeout"))
                }
            }
        } else {
            tokio::select! {
                r = handler.handle(&ctx) => r,
                _ = signal.cancelled() => {
                    Err(aborted_error("signal"))
                }
            }
        };

        // Stop lock renewal.
        renew_handle.abort();

        // Dispatch result (same complete/fail logic as execute_job).
        match result {
            Ok(value) => {
                let mapped = map_result(value);
                queue
                    .complete_job(job.id, &lock_token, mapped.as_ref())
                    .await?;
                jobs_completed.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(err) => {
                Self::handle_failure(&queue, &job, &lock_token, &err).await?;
                Ok(())
            }
        }
    }

    /// Dispatch one claimed job to its handler and drive the resulting
    /// transition. Mirrors TS `executeJob` (`worker.ts` L673-847), minus the
    /// slices deferred out of 1-2-2 (lock-renew timer, per-job timeout,
    /// abort-derived reasons, lease-full bounce).
    async fn execute_job(&self, job: MinionJob, lock_token: String) -> Result<()> {
        let queue = MinionQueue::new(&*self.engine);

        // No handler for this name -> dead-letter (TS L679-683). claim only
        // returns registered names, so this is a belt-and-suspenders guard for
        // a handler deregistered between claim and dispatch.
        let Some(handler) = self.handlers.get(&job.name).cloned() else {
            queue
                .fail_job(
                    job.id,
                    &lock_token,
                    &format!("No handler for job type '{}'", job.name),
                    FailOutcome::Dead,
                    0,
                )
                .await?;
            return Ok(());
        };

        // Per-job cancellation signal. In 1-2-2 nothing fires it (no lock timer
        // / timeout yet); it exists so the handler contract is whole. 1-2-3
        // wires renew-loss / timeout / cancel into it.
        let signal = CancellationToken::new();
        let ctx = MinionJobContext::new(
            Arc::clone(&self.engine),
            job.id,
            job.name.clone(),
            job.data.clone(),
            job.attempts_made,
            lock_token.clone(),
            signal,
            self.shutdown.clone(),
        );

        match handler.handle(&ctx).await {
            Ok(result) => {
                // Result mapping (roadmap 1-2-2 decision 2, faithful to TS
                // L730-734): object/array stored as-is; a scalar is wrapped
                // `{"value": x}`; JSON null means "no result".
                let mapped = map_result(result);
                queue
                    .complete_job(job.id, &lock_token, mapped.as_ref())
                    .await?;
                // A dropped completion (token mismatch -> None) means the job
                // was reclaimed; nothing more to do (TS L736-739).
                Ok(())
            }
            Err(err) => {
                Self::handle_failure(&queue, &job, &lock_token, &err).await?;
                Ok(())
            }
        }
    }

    /// Route a handler error to dead/delayed with backoff. Mirrors TS L814-845
    /// (the lease-full bounce branch at L777-812 is deferred).
    async fn handle_failure(
        queue: &MinionQueue<'_>,
        job: &MinionJob,
        lock_token: &str,
        err: &zbrain_core::error::StructuredError,
    ) -> Result<()> {
        let is_unrecoverable = err.class == UNRECOVERABLE_KIND;
        // attempts_made on the claimed snapshot is the count BEFORE this run;
        // the run about to be recorded is attempt (attempts_made + 1). fail_job
        // itself increments the stored counter, so we judge exhaustion here on
        // the +1 (TS L815).
        let attempts_exhausted = job.attempts_made + 1 >= job.max_attempts;

        let outcome = if is_unrecoverable || attempts_exhausted {
            FailOutcome::Dead
        } else {
            FailOutcome::Delayed
        };

        let backoff_ms = if outcome == FailOutcome::Delayed {
            calculate_backoff(&BackoffInput {
                backoff_type: job.backoff_type,
                backoff_delay: job.backoff_delay as i64,
                backoff_jitter: job.backoff_jitter,
                attempts_made: job.attempts_made + 1,
            })
            .round() as i64
        } else {
            0
        };

        queue
            .fail_job(job.id, lock_token, &err.message, outcome, backoff_ms)
            .await?;
        Ok(())
    }
}

/// Build an "aborted: <reason>" error for signal-driven cancellation (timeout,
/// lock-loss). Mirrors TS L754-758 where `abort.signal.reason` is surfaced as
/// `errorText = "aborted: <reason>"`.
fn aborted_error(reason: &str) -> zbrain_core::error::StructuredError {
    zbrain_core::error::StructuredError::new("aborted", "worker", format!("aborted: {reason}"))
}

/// Map a handler's return value to the stored `job.result`, faithful to the TS
/// `executeJob` completion (`worker.ts` L730-734):
/// - `null` -> `None` (no result column set)
/// - object / array -> stored verbatim
/// - any other scalar (string / number / bool) -> wrapped `{"value": x}`
fn map_result(result: Value) -> Option<Value> {
    match result {
        Value::Null => None,
        v @ (Value::Object(_) | Value::Array(_)) => Some(v),
        scalar => Some(json!({ "value": scalar })),
    }
}

/// Monotonic-ish millisecond stamp for lock-token uniqueness. Not a clock the
/// queue reasons about — just entropy for the token string.
fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Cheap unique-ish worker id without pulling `uuid` into this crate. Combines
/// a time stamp with the thread id; the worker id only needs to distinguish
/// concurrent workers' lock tokens, not be a real UUID.
fn uuid_like() -> String {
    format!("worker-{}", now_millis())
}
