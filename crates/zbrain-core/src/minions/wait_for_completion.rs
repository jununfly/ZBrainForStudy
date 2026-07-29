//! Poll-until-terminal helper for minion job callers.
//!
//! Port of TS `src/core/minions/wait-for-completion.ts`. The minion worker
//! side has no notification stream for arbitrary callers, so callers that want
//! to block on a job (e.g. the `patterns` cycle phase, which enqueues a single
//! subagent and waits for it) poll [`MinionQueue::get_job`] until the job
//! reaches a terminal state.
//!
//! On timeout the job is NOT cancelled — the caller can inspect it later.
//! Explicit cancellation is the caller's responsibility.

use std::time::Duration;

use crate::minions::queue::MinionQueue;
use crate::minions::types::{MinionJob, MinionJobStatus};

/// Error returned by [`wait_for_completion`].
#[derive(Debug)]
pub enum WaitError {
    /// The job did not reach a terminal state within `timeout_ms`.
    Timeout { job_id: i64, elapsed_ms: u64 },
    /// The job disappeared (was deleted) while waiting.
    NotFound(i64),
    /// Underlying engine error while polling.
    Engine(crate::Error),
}

impl std::fmt::Display for WaitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WaitError::Timeout { job_id, elapsed_ms } => {
                write!(f, "timeout after {elapsed_ms}ms waiting for job {job_id}")
            }
            WaitError::NotFound(id) => write!(f, "job {id} not found while waiting"),
            WaitError::Engine(e) => write!(f, "engine error while waiting for job: {e}"),
        }
    }
}

impl std::error::Error for WaitError {}

/// Options for [`wait_for_completion`].
#[derive(Debug, Clone, Default)]
pub struct WaitOpts {
    /// Abort after this many ms. Default: 24h.
    pub timeout_ms: Option<u64>,
    /// Poll interval in ms. Default: 1000ms.
    pub poll_ms: Option<u64>,
}

const TERMINAL_STATES: &[MinionJobStatus] = &[
    MinionJobStatus::Completed,
    MinionJobStatus::Failed,
    MinionJobStatus::Dead,
    MinionJobStatus::Cancelled,
];

fn is_terminal(status: MinionJobStatus) -> bool {
    TERMINAL_STATES.contains(&status)
}

/// Wait for `job_id` to reach a terminal state, polling [`MinionQueue::get_job`].
///
/// Returns the final [`MinionJob`] snapshot. On timeout, returns
/// [`WaitError::Timeout`]. If the job vanishes mid-wait, returns
/// [`WaitError::NotFound`].
pub async fn wait_for_completion(
    queue: &MinionQueue<'_>,
    job_id: i64,
    opts: WaitOpts,
) -> Result<MinionJob, WaitError> {
    let timeout_ms = opts.timeout_ms.unwrap_or(24 * 60 * 60 * 1000);
    let poll_ms = opts.poll_ms.unwrap_or(1000);

    // Fast-path: don't wait a full poll interval just to learn it's done.
    let mut job = queue
        .get_job(job_id)
        .await
        .map_err(WaitError::Engine)?
        .ok_or(WaitError::NotFound(job_id))?;
    if is_terminal(job.status) {
        return Ok(job);
    }

    let started = std::time::Instant::now();
    loop {
        if started.elapsed().as_millis() as u64 >= timeout_ms {
            return Err(WaitError::Timeout {
                job_id,
                elapsed_ms: started.elapsed().as_millis() as u64,
            });
        }
        let sleep = poll_ms.min(timeout_ms - started.elapsed().as_millis() as u64);
        tokio::time::sleep(Duration::from_millis(sleep)).await;

        job = queue
            .get_job(job_id)
            .await
            .map_err(WaitError::Engine)?
            .ok_or(WaitError::NotFound(job_id))?;
        if is_terminal(job.status) {
            return Ok(job);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{BrainEngine, EngineConfig, InMemoryEngine};
    use crate::minions::queue::MinionQueue;
    use crate::minions::types::{MinionJobInput, MinionJobStatus};

    // InMemory implements both enqueue_job and get_job (get_job returns
    // Ok(None)/Ok(Some(job)) — never Err), so waiting against an InMemory
    // engine either resolves to NotFound (no such job) or polls until timeout
    // (job present but no worker runs it). Use libsql for a real job row whose
    // status can transition, and InMemory only for the NotFound path.
    static LIBSQL_TEST_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> =
        std::sync::OnceLock::new();
    fn libsql_guard() -> std::sync::MutexGuard<'static, ()> {
        LIBSQL_TEST_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    async fn libsql_engine() -> (tempfile::NamedTempFile, crate::libsql::LibsqlEngine) {
        let temp = tempfile::NamedTempFile::new().expect("temp db");
        let path = temp.path().to_string_lossy().to_string();
        let config = EngineConfig {
            database_path: Some(path),
            database_url: None,
        };
        let engine = crate::libsql::LibsqlEngine::new();
        engine.connect(&config).await.unwrap();
        engine.init_schema().await.unwrap();
        (temp, engine)
    }

    #[tokio::test]
    async fn wait_for_completion_times_out() {
        let _g = libsql_guard();
        let (_temp, engine) = libsql_engine().await;
        let queue = MinionQueue::new(&engine);
        let job = queue
            .add(&MinionJobInput {
                name: "subagent".into(),
                data: Some(serde_json::json!({"prompt": "x"})),
                ..Default::default()
            })
            .await
            .unwrap();
        // No worker runs, so the job stays Waiting → wait_for_completion times out.
        let result = wait_for_completion(
            &queue,
            job.id,
            WaitOpts {
                timeout_ms: Some(120),
                poll_ms: Some(20),
            },
        )
        .await;
        match result {
            Err(WaitError::Timeout { job_id, .. }) => assert_eq!(job_id, job.id),
            other => panic!("expected Timeout, got {other:?}"),
        }
        // The job is still Waiting (never reached a terminal state).
        let j = queue.get_job(job.id).await.unwrap().unwrap();
        assert_eq!(j.status, MinionJobStatus::Waiting);
    }

    #[tokio::test]
    async fn wait_for_completion_not_found() {
        let _g = libsql_guard();
        let (_temp, engine) = libsql_engine().await;
        let queue = MinionQueue::new(&engine);
        let result = wait_for_completion(
            &queue,
            9_999_999,
            WaitOpts {
                timeout_ms: Some(50),
                poll_ms: Some(10),
            },
        )
        .await;
        assert!(matches!(result, Err(WaitError::NotFound(9_999_999))));
    }

    // InMemory get_job returns Ok(None) for an unknown job → maps to
    // WaitError::NotFound (it never errors on InMemory).
    #[tokio::test]
    async fn wait_for_completion_inmemory_missing_returns_not_found() {
        let engine = InMemoryEngine::new();
        engine.connect(&EngineConfig::default()).await.unwrap();
        let queue = MinionQueue::new(&engine);
        let result = wait_for_completion(
            &queue,
            1,
            WaitOpts {
                timeout_ms: Some(50),
                poll_ms: Some(10),
            },
        )
        .await;
        assert!(matches!(result, Err(WaitError::NotFound(1))));
    }
}
