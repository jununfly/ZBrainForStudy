//! In-process subagent executor for CLI commands.
//!
//! The Rust CLI otherwise has **no** code path that dequeues and runs queued
//! minion jobs: `agent run` and `jobs work` only `queue.add()` a row and poll,
//! so a submitted `subagent` job never executes unless an external worker is
//! already running (and none is started anywhere in production). Commands that
//! must be self-contained — fan out N jobs, wait, then read their results in
//! one process — need to run a worker themselves.
//!
//! [`run_subagent_jobs`] is that executor: it spins up a short-lived
//! [`MinionWorker`] with a [`SubagentHandler`] registered, drives the queue
//! until every target job id reaches a terminal state (or an overall deadline
//! elapses), shuts the worker down, and returns the final job rows in the same
//! order as `job_ids`.
//!
//! Both `book-mirror` (fan-out) and `agent run --follow` (single job) build on
//! it. It is deliberately not a daemon: the health-check + RSS watchdog (which
//! exist to reap long-lived daemon workers) are disabled for a one-shot batch.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use zbrain_core::ai::chat::ChatProvider;
use zbrain_core::engine::BrainEngine;
use zbrain_core::minions::handlers::subagent::SubagentHandler;
use zbrain_core::minions::queue::MinionQueue;
use zbrain_core::minions::types::MinionJob;
use zbrain_core::minions::MinionWorkerOpts;
use zbrain_worker::MinionWorker;

/// Tuning knobs for a one-shot inline worker run.
pub struct InlineWorkerOpts {
    /// Max concurrent in-flight jobs. Clamped to at least 1.
    pub concurrency: u32,
    /// Poll cadence in ms for both the worker's idle sleep and the completion
    /// poll below.
    pub poll_ms: u64,
    /// Overall wall-clock ceiling for the whole batch. A safety bound on top of
    /// each job's own `timeout_ms`; when hit, remaining non-terminal jobs are
    /// left as-is and their rows are returned in whatever state they reached.
    pub overall_deadline: Duration,
}

impl Default for InlineWorkerOpts {
    fn default() -> Self {
        Self {
            concurrency: 4,
            poll_ms: 250,
            overall_deadline: Duration::from_secs(30 * 60),
        }
    }
}

/// Execute the `subagent` jobs identified by `job_ids` in-process until all are
/// terminal (or `overall_deadline` elapses), then return the final job rows in
/// the same order as `job_ids` (an entry is `None` if the row vanished).
///
/// The caller is responsible for having already enqueued the jobs (via
/// [`MinionQueue::add`]) — this only runs them.
pub async fn run_subagent_jobs(
    engine: Arc<dyn BrainEngine>,
    provider: Arc<dyn ChatProvider>,
    job_ids: &[i64],
    opts: InlineWorkerOpts,
) -> zbrain_core::Result<Vec<Option<MinionJob>>> {
    let worker_opts = MinionWorkerOpts {
        concurrency: opts.concurrency.max(1),
        poll_interval_ms: opts.poll_ms as i64,
        // One-shot batch: disable the daemon-oriented supervisors so a short
        // run doesn't get reaped by the stall detector or RSS watchdog.
        health_check_interval_ms: 0,
        max_rss_mb: None,
        ..Default::default()
    };
    let mut worker = MinionWorker::new(Arc::clone(&engine), worker_opts);
    worker.register("subagent", Arc::new(SubagentHandler::new(provider)));
    let shutdown = worker.shutdown_token();

    // Run the consume loop in the background; it stops when we cancel `shutdown`.
    let worker = Arc::new(worker);
    let worker_task = {
        let worker = Arc::clone(&worker);
        tokio::spawn(async move { worker.run().await })
    };

    // Poll every target id until all terminal or the deadline is reached.
    let deadline = Instant::now() + opts.overall_deadline;
    loop {
        let queue = MinionQueue::new(&*engine);
        let mut all_terminal = true;
        for &id in job_ids {
            match queue.get_job(id).await? {
                Some(job) if job.status.is_terminal() => {}
                _ => {
                    all_terminal = false;
                    break;
                }
            }
        }
        if all_terminal || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(opts.poll_ms)).await;
    }

    // Stop the worker and let in-flight jobs drain.
    shutdown.cancel();
    let _ = worker_task.await;

    // Collect final rows in caller order.
    let queue = MinionQueue::new(&*engine);
    let mut out = Vec::with_capacity(job_ids.len());
    for &id in job_ids {
        out.push(queue.get_job(id).await?);
    }
    Ok(out)
}
