//! Bounded in-memory queue for fact extraction — the Rust port of
//! `src/core/facts/queue.ts`.
//!
//! Design notes (mirrors the TS contract, /plan-eng-review D6 + D7):
//!   - Cap on pending entries; drop-oldest on overflow, counting the drop.
//!   - Per-session in-flight cap (default 1) serializes extraction within a
//!     session so a burst of chat doesn't fan out parallel extractions.
//!   - Cooperative cancellation via [`tokio_util::sync::CancellationToken`]
//!     (the Rust analog of `AbortSignal`). On shutdown the internal token is
//!     cancelled; jobs must poll [`CancellationToken::is_cancelled`] to
//!     cooperate.
//!   - Shutdown: stop accepting new entries, best-effort grace drain of
//!     in-flight, then drop remaining pending (counted as `dropped_shutdown`).
//!
//! The queue is a process singleton via [`get_facts_queue`]; tests inject a
//! fresh instance and reset the singleton with [`reset_facts_queue_for_tests`].
//!
//! The queue takes opaque jobs `(CancellationToken) -> Future` so callers
//! compose the actual extraction pipeline. The queue's only job is order +
//! concurrency + dropping under load.
//!
//! State is protected by a `std::sync::Mutex` (small, never held across an
//! `.await`), so `enqueue` stays a synchronous fire-and-forget call matching
//! the TS signature while remaining safe to invoke from async contexts.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// Counters exposed for operator visibility (e.g. `zbrain doctor`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FactsQueueCounters {
    pub enqueued: u64,
    pub completed: u64,
    pub dropped_overflow: u64,
    pub dropped_shutdown: u64,
    pub failed: u64,
}

/// Tunables for a [`FactsQueue`] instance.
#[derive(Debug, Clone, Copy)]
pub struct FactsQueueOpts {
    /// Max pending jobs in the queue. Defaults to 100.
    pub cap: usize,
    /// Per-session in-flight cap. Defaults to 1 (serialized).
    pub per_session_inflight_cap: usize,
    /// Grace ms for in-flight to drain on shutdown. Defaults to 5000.
    pub shutdown_grace_ms: u64,
}

impl Default for FactsQueueOpts {
    fn default() -> Self {
        Self {
            cap: 100,
            per_session_inflight_cap: 1,
            shutdown_grace_ms: 5000,
        }
    }
}

/// A job body. The caller decides what runs given a cancellation token.
/// Must be cooperatively cancellable (poll `token.is_cancelled()`).
///
/// Returns `Ok(())` on success; an `Err` is counted as a `failed` (non-abort)
/// completion, mirroring the TS `runEntry` catch branch.
pub type FactsJob = Box<dyn FnOnce(CancellationToken) -> JobFuture + Send + 'static>;

/// Boxed future returned by a [`FactsJob`].
pub type JobFuture = Pin<
    Box<
        dyn Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>>
            + Send
            + 'static,
    >,
>;

struct QueueEntry {
    job: FactsJob,
    session_id: String,
}

struct QState {
    opts: FactsQueueOpts,
    pending: VecDeque<QueueEntry>,
    /// Per-session in-flight count.
    inflight_by_session: HashMap<String, usize>,
    /// Global in-flight count (for shutdown drain accounting).
    inflight_total: usize,
    counters: FactsQueueCounters,
    shutting_down: bool,
    /// Internal cancellation token — cancelled on shutdown.
    abort: CancellationToken,
    /// True while a pump task is live, so we don't spawn duplicates.
    pumping: bool,
}

/// Bounded in-memory queue for fact extraction.
///
/// Cheap to clone (all state is behind `Arc`). Holds an internal
/// [`CancellationToken`] for shutdown propagation.
#[derive(Clone)]
pub struct FactsQueue {
    state: Arc<StdMutex<QState>>,
}

impl FactsQueue {
    /// Create a fresh queue with the given options (or defaults).
    pub fn new(opts: FactsQueueOpts) -> Self {
        let opts = FactsQueueOpts {
            cap: opts.cap.max(1),
            per_session_inflight_cap: opts.per_session_inflight_cap.max(1),
            shutdown_grace_ms: opts.shutdown_grace_ms,
        };
        Self {
            state: Arc::new(StdMutex::new(QState {
                opts,
                pending: VecDeque::new(),
                inflight_by_session: HashMap::new(),
                inflight_total: 0,
                counters: FactsQueueCounters::default(),
                shutting_down: false,
                abort: CancellationToken::new(),
                pumping: false,
            })),
        }
    }

    /// Enqueue a job. Returns the pending depth after insertion, or `-1` if the
    /// job was dropped because the queue is shutting down. Drop-oldest-on-
    /// overflow when `cap` is hit.
    ///
    /// This is synchronous from the caller's perspective: the pump is scheduled
    /// on a tokio task (the analog of TS `queueMicrotask`).
    pub fn enqueue(&self, job: FactsJob, session_id: String) -> i64 {
        let mut st = self.state.lock().expect("facts-queue state poisoned");
        if st.shutting_down {
            st.counters.dropped_shutdown += 1;
            return -1;
        }
        if st.pending.len() >= st.opts.cap {
            // Drop oldest. The dropped job's handler is never invoked; callers
            // upstream of the queue treat enqueue() as fire-and-forget and
            // monitor counters for capacity pressure.
            st.pending.pop_front();
            st.counters.dropped_overflow += 1;
        }
        st.pending.push_back(QueueEntry { job, session_id });
        st.counters.enqueued += 1;
        let depth = st.pending.len() as i64;
        let was_pumping = st.pumping;
        if !was_pumping {
            st.pumping = true;
            drop(st);
            let this = Arc::new(self.clone());
            tokio::spawn(this.pump());
        }
        depth
    }

    /// Snapshot of the counters.
    pub fn get_counters(&self) -> FactsQueueCounters {
        self.state.lock().expect("facts-queue state poisoned").counters
    }

    /// Pending depth (queued but not yet picked up).
    pub fn pending_count(&self) -> usize {
        self.state.lock().expect("facts-queue state poisoned").pending.len()
    }

    /// In-flight count across all sessions.
    pub fn inflight_count(&self) -> usize {
        self.state
            .lock()
            .expect("facts-queue state poisoned")
            .inflight_total
    }

    /// Begin shutdown. Resolves once the queue has either fully drained
    /// in-flight (under `shutdown_grace_ms`) OR the grace expired. After this
    /// resolves, all pending jobs are dropped with `dropped_shutdown` counted.
    pub async fn shutdown(&self) {
        let grace = {
            let mut st = self.state.lock().expect("facts-queue state poisoned");
            if st.shutting_down {
                return;
            }
            st.shutting_down = true;
            st.abort.cancel();
            st.opts.shutdown_grace_ms
        };
        let self2 = Arc::new(self.clone());
        // The lock is released before spawning so the drain task can acquire it.
        let handle = tokio::spawn(async move {
            let start = std::time::Instant::now();
            loop {
                {
                    let s = self2.state.lock().expect("facts-queue state poisoned");
                    if s.inflight_total == 0 {
                        break;
                    }
                    if start.elapsed().as_millis() as u64 >= grace {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            let mut s = self2.state.lock().expect("facts-queue state poisoned");
            let dropped = s.pending.len();
            s.pending.clear();
            s.counters.dropped_shutdown += dropped as u64;
        });
        let _ = handle.await;
    }

    /// Pump: pick up entries respecting the per-session in-flight cap. Loops so
    /// a single released slot can unblock multiple sessions.
    ///
    /// Returns a boxed future (not `async fn`) so the recursive
    /// `tokio::spawn(self.pump())` call does not create a self-referential
    /// opaque-type cycle (E0391).
    fn pump(self: Arc<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            loop {
                let entry = {
                    let mut st = self.state.lock().expect("facts-queue state poisoned");
                    if st.shutting_down {
                        st.pumping = false;
                        return;
                    }
                    let mut idx = None;
                    for (i, e) in st.pending.iter().enumerate() {
                        let inflight =
                            st.inflight_by_session.get(&e.session_id).copied().unwrap_or(0);
                        if inflight < st.opts.per_session_inflight_cap {
                            idx = Some(i);
                            break;
                        }
                    }
                    match idx {
                        Some(i) => {
                            let e = st.pending.remove(i).expect("idx in range");
                            let n = st
                                .inflight_by_session
                                .get(&e.session_id)
                                .copied()
                                .unwrap_or(0)
                                + 1;
                            st.inflight_by_session.insert(e.session_id.clone(), n);
                            st.inflight_total += 1;
                            Some(e)
                        }
                        None => {
                            st.pumping = false;
                            return;
                        }
                    }
                };
                match entry {
                    Some(e) => {
                        let this = Arc::new(self.clone());
                        let token = self.state.lock().expect("facts-queue state poisoned").abort.clone();
                        tokio::spawn(async move {
                            this.run_entry(e, token).await;
                        });
                        // Loop to try the next eligible entry (possibly a
                        // different session) before yielding.
                    }
                    None => return,
                }
            }
        })
    }

    async fn run_entry(&self, entry: QueueEntry, token: CancellationToken) {
        let result: Result<(), Box<dyn std::error::Error + Send + Sync>> = {
            let fut = (entry.job)(token);
            fut.await
        };
        match result {
            Ok(()) => {
                self.state
                    .lock()
                    .expect("facts-queue state poisoned")
                    .counters
                    .completed += 1;
            }
            Err(err) => {
                let mut st = self.state.lock().expect("facts-queue state poisoned");
                if st.abort.is_cancelled() {
                    // Aborted (shutdown) — count as a shutdown drop, not a fault.
                    st.counters.dropped_shutdown += 1;
                } else {
                    st.counters.failed += 1;
                    tracing::warn!(
                        "[facts-queue] job failed for session={}: {err}",
                        entry.session_id
                    );
                }
            }
        }
        // Release the in-flight slot.
        {
            let mut st = self.state.lock().expect("facts-queue state poisoned");
            let remaining = st
                .inflight_by_session
                .get(&entry.session_id)
                .copied()
                .unwrap_or(1)
                - 1;
            if remaining <= 0 {
                st.inflight_by_session.remove(&entry.session_id);
            } else {
                st.inflight_by_session.insert(entry.session_id.clone(), remaining);
            }
            st.inflight_total -= 1;
        }
        // A released slot may unblock a pending entry — kick the pump.
        let need_pump = {
            let mut st = self.state.lock().expect("facts-queue state poisoned");
            if st.pumping {
                false
            } else {
                st.pumping = true;
                true
            }
        };
        if need_pump {
            let this = Arc::new(self.clone());
            tokio::spawn(this.pump());
        }
    }
}

// ── Process-singleton ──────────────────────────────────────

static SINGLETON: StdMutex<Option<Arc<FactsQueue>>> = StdMutex::new(None);

/// Lazily initialize (with sensible defaults) and return the process-wide
/// queue. Tests should call [`reset_facts_queue_for_tests`] first to get a
/// fresh instance.
pub fn get_facts_queue(opts: Option<FactsQueueOpts>) -> Arc<FactsQueue> {
    let mut guard = SINGLETON.lock().expect("facts-queue singleton poisoned");
    match guard.as_ref() {
        Some(q) => q.clone(),
        None => {
            let q = Arc::new(FactsQueue::new(opts.unwrap_or_default()));
            *guard = Some(q.clone());
            q
        }
    }
}

/// Test helper: reset the process-level singleton so the next
/// [`get_facts_queue`] call builds a fresh instance.
pub fn reset_facts_queue_for_tests() {
    *SINGLETON.lock().expect("facts-queue singleton poisoned") = None;
}

/// Small helper for tests: await until `pred` is true or the timeout elapses.
#[cfg(test)]
async fn wait_until<F: Fn() -> bool>(pred: F, ms: u64) {
    let deadline = std::time::Instant::now() + Duration::from_millis(ms);
    while !pred() {
        if std::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_job() -> FactsJob {
        Box::new(|_token| Box::pin(async move { Ok(()) }))
    }

    #[tokio::test]
    async fn enqueue_increments_counters_and_completes() {
        // Use a local instance (not the process singleton) so tests don't
        // race on shared global state when run in parallel.
        let q = FactsQueue::new(FactsQueueOpts {
            cap: 100,
            per_session_inflight_cap: 1,
            shutdown_grace_ms: 5000,
        });

        let depth = q.enqueue(noop_job(), "s1".to_string());
        assert_eq!(depth, 1);
        wait_until(|| q.get_counters().completed >= 1, 1000).await;

        let c = q.get_counters();
        assert_eq!(c.enqueued, 1);
        assert_eq!(c.completed, 1);
        assert_eq!(q.pending_count(), 0);
    }

    #[tokio::test]
    async fn drop_oldest_on_overflow() {
        // gate keeps jobs in-flight so they never free their slot
        let gate = Arc::new(Notify::new());
        let q = FactsQueue::new(FactsQueueOpts {
            cap: 2,
            per_session_inflight_cap: 1,
            shutdown_grace_ms: 5000,
        });

        // 5 enqueues, all blocked in-flight/pending (single session, cap 1).
        for _ in 0..5 {
            let gate = gate.clone();
            q.enqueue(
                Box::new(move |_token| Box::pin(async move { gate.notified().await; Ok(()) })),
                "s1".to_string(),
            );
        }

        // Under concurrent pump execution the exact dropped_overflow count is
        // timing-dependent (as it is in the TS original), but the contract
        // invariants must hold: pending stays bounded by cap, overflow drops
        // happened, and enqueued == completed + inflight + pending + dropped.
        let c = q.get_counters();
        assert_eq!(c.enqueued, 5);
        assert!(q.pending_count() <= 2, "pending must stay bounded by cap");
        assert!(
            c.dropped_overflow >= 2,
            "overflow should drop at least 2 oldest (got {})",
            c.dropped_overflow
        );
        let accounted = c.completed + q.inflight_count() as u64 + q.pending_count() as u64 + c.dropped_overflow;
        assert_eq!(accounted, c.enqueued, "counter conservation");
    }

    #[tokio::test]
    async fn shutdown_rejects_new_and_drops_pending() {
        let gate = Arc::new(Notify::new());
        let q = FactsQueue::new(FactsQueueOpts {
            cap: 100,
            per_session_inflight_cap: 1,
            shutdown_grace_ms: 200,
        });
        q.enqueue(
            Box::new(move |_token| {
                let gate = gate.clone();
                Box::pin(async move { gate.notified().await; Ok(()) })
            }),
            "s1".to_string(),
        );
        q.enqueue(noop_job(), "s1".to_string()); // pending

        q.shutdown().await;

        // New enqueues during/after shutdown are rejected.
        let rejected = q.enqueue(noop_job(), "s1".to_string());
        assert_eq!(rejected, -1);
        let c = q.get_counters();
        assert!(c.dropped_shutdown >= 1, "pending should be dropped on shutdown");
    }

    #[tokio::test]
    async fn per_session_serialized_inflight_cap_one() {
        // A shared flag proves no two jobs for the same session overlap.
        let overlap = Arc::new(StdMutex::new(false));
        let saw_overlap = Arc::new(StdMutex::new(false));
        let release = Arc::new(Notify::new());

        let q = FactsQueue::new(FactsQueueOpts {
            cap: 100,
            per_session_inflight_cap: 1,
            shutdown_grace_ms: 5000,
        });

        for _ in 0..3 {
            let overlap = overlap.clone();
            let saw_overlap = saw_overlap.clone();
            let release = release.clone();
            q.enqueue(
                Box::new(move |_token| {
                    let overlap = overlap.clone();
                    let saw_overlap = saw_overlap.clone();
                    let release = release.clone();
                    Box::pin(async move {
                        {
                            let mut guard = overlap.lock().unwrap();
                            if *guard {
                                *saw_overlap.lock().unwrap() = true;
                            }
                            *guard = true;
                        }
                        release.notified().await;
                        {
                            let mut guard = overlap.lock().unwrap();
                            *guard = false;
                        }
                        Ok(())
                    })
                }),
                "same".to_string(),
            );
        }

        // Let them run and resolve one-by-one.
        for _ in 0..3 {
            release.notify_one();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !*saw_overlap.lock().unwrap(),
            "per-session cap=1 must prevent overlapping jobs"
        );
    }

    #[tokio::test]
    async fn failed_job_increments_failed_counter() {
        let q = FactsQueue::new(FactsQueueOpts::default());
        q.enqueue(
            Box::new(|_token| Box::pin(async move { Err("boom".into()) })),
            "s1".to_string(),
        );
        wait_until(|| q.get_counters().failed >= 1, 1000).await;
        assert_eq!(q.get_counters().failed, 1);
        assert_eq!(q.get_counters().completed, 0);
    }
}
