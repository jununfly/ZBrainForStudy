//! Phase 9 slice 1-2-2: minion worker queue-consume-core integration tests.
//!
//! Drives `MinionWorker::process_next_job` end-to-end against `InMemoryEngine`:
//! empty-queue idle, success + result mapping, retry-with-backoff,
//! unrecoverable -> dead, attempts-exhausted -> dead, and the
//! promote-delayed -> claim closed loop. Serial (concurrency = 1) matches the
//! slice scope; concurrency + signals land in 1-2-3.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use zbrain_core::engine::BrainEngine;
use zbrain_core::minions::types::{BackoffType, MinionJobInput, MinionJobStatus};
use zbrain_core::minions::{MinionHandler, MinionJobContext, MinionWorkerOpts};
use zbrain_core::InMemoryEngine;
use zbrain_worker::{unrecoverable, MinionWorker, ProcessOutcome};

// --- Test handlers ---------------------------------------------------------

/// Always succeeds, returning a fixed JSON value.
struct Succeed(Value);
#[async_trait]
impl MinionHandler for Succeed {
    async fn handle(&self, _ctx: &MinionJobContext) -> zbrain_core::Result<Value> {
        Ok(self.0.clone())
    }
}

/// Always fails with a plain (retryable) error.
struct Boom;
#[async_trait]
impl MinionHandler for Boom {
    async fn handle(&self, _ctx: &MinionJobContext) -> zbrain_core::Result<Value> {
        Err(zbrain_core::error::StructuredError::new(
            "Handler", "handler", "boom",
        ))
    }
}

/// Always fails with an unrecoverable error (should not be retried).
struct Fatal;
#[async_trait]
impl MinionHandler for Fatal {
    async fn handle(&self, _ctx: &MinionJobContext) -> zbrain_core::Result<Value> {
        Err(unrecoverable("do not retry me"))
    }
}

// --- Helpers ---------------------------------------------------------------

fn worker(engine: Arc<dyn BrainEngine>) -> MinionWorker {
    MinionWorker::new(engine, MinionWorkerOpts::default())
}

async fn enqueue(engine: &dyn BrainEngine, input: MinionJobInput) -> i64 {
    engine.enqueue_job(&input).await.expect("enqueue").id
}

fn job(name: &str) -> MinionJobInput {
    MinionJobInput {
        name: name.to_string(),
        ..Default::default()
    }
}

// --- behavior 4: empty queue -> Idle ---------------------------------------

#[tokio::test]
async fn empty_queue_is_idle() {
    let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
    let mut w = worker(Arc::clone(&engine));
    w.register("noop", Arc::new(Succeed(Value::Null)));

    let outcome = w.process_next_job().await.expect("process");
    assert_eq!(outcome, ProcessOutcome::Idle);
}

#[tokio::test]
async fn no_registered_handler_claims_nothing() {
    let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
    enqueue(&*engine, job("build")).await;
    // Worker registers nothing -> registered_names empty -> claim no-op -> Idle,
    // and the waiting job stays waiting (not dead-lettered).
    let w = worker(Arc::clone(&engine));
    assert_eq!(w.process_next_job().await.unwrap(), ProcessOutcome::Idle);

    let jobs = engine.get_jobs(&Default::default()).await.expect("get_jobs");
    assert_eq!(jobs[0].status, MinionJobStatus::Waiting);
}

// --- behavior 5: success + object result stored verbatim -------------------

#[tokio::test]
async fn successful_job_completes_with_object_result() {
    let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
    let id = enqueue(&*engine, job("build")).await;

    let mut w = worker(Arc::clone(&engine));
    w.register("build", Arc::new(Succeed(json!({"ok": true, "n": 3}))));

    assert_eq!(
        w.process_next_job().await.unwrap(),
        ProcessOutcome::Processed(id)
    );

    let stored = engine.get_job(id).await.unwrap().unwrap();
    assert_eq!(stored.status, MinionJobStatus::Completed);
    assert_eq!(stored.result, Some(json!({"ok": true, "n": 3})));
}

// --- behavior 6: result mapping (scalar wraps, null -> none) ----------------

#[tokio::test]
async fn scalar_result_is_wrapped_in_value_key() {
    let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
    let id = enqueue(&*engine, job("calc")).await;

    let mut w = worker(Arc::clone(&engine));
    w.register("calc", Arc::new(Succeed(json!(42))));
    w.process_next_job().await.unwrap();

    let stored = engine.get_job(id).await.unwrap().unwrap();
    assert_eq!(stored.result, Some(json!({"value": 42})));
}

#[tokio::test]
async fn null_result_stores_no_result() {
    let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
    let id = enqueue(&*engine, job("ping")).await;

    let mut w = worker(Arc::clone(&engine));
    w.register("ping", Arc::new(Succeed(Value::Null)));
    w.process_next_job().await.unwrap();

    let stored = engine.get_job(id).await.unwrap().unwrap();
    assert_eq!(stored.status, MinionJobStatus::Completed);
    assert_eq!(stored.result, None);
}

// --- behavior 8: retryable failure -> delayed + backoff + attempt burned ---

#[tokio::test]
async fn retryable_failure_delays_with_backoff() {
    let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
    let id = enqueue(
        &*engine,
        MinionJobInput {
            name: "flaky".to_string(),
            max_attempts: Some(3),
            backoff_type: Some(BackoffType::Fixed),
            backoff_delay: Some(1000),
            backoff_jitter: Some(0.0),
            ..Default::default()
        },
    )
    .await;

    let mut w = worker(Arc::clone(&engine));
    w.register("flaky", Arc::new(Boom));
    w.process_next_job().await.unwrap();

    let stored = engine.get_job(id).await.unwrap().unwrap();
    assert_eq!(stored.status, MinionJobStatus::Delayed);
    assert_eq!(stored.attempts_made, 1); // fail_job burned one attempt
    assert!(stored.delay_until.is_some(), "delayed retry sets delay_until");
    assert_eq!(stored.error_text.as_deref(), Some("boom"));
}

// --- behavior 9: unrecoverable failure -> dead immediately -----------------

#[tokio::test]
async fn unrecoverable_failure_goes_dead_without_retry() {
    let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
    let id = enqueue(
        &*engine,
        MinionJobInput {
            name: "fatal".to_string(),
            max_attempts: Some(5), // plenty of attempts left...
            ..Default::default()
        },
    )
    .await;

    let mut w = worker(Arc::clone(&engine));
    w.register("fatal", Arc::new(Fatal));
    w.process_next_job().await.unwrap();

    let stored = engine.get_job(id).await.unwrap().unwrap();
    // ...but unrecoverable bypasses the retry curve entirely.
    assert_eq!(stored.status, MinionJobStatus::Dead);
    assert!(stored.delay_until.is_none());
}

// --- behavior 10: attempts exhausted -> dead even for retryable error ------

#[tokio::test]
async fn last_attempt_failure_goes_dead() {
    let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
    let id = enqueue(
        &*engine,
        MinionJobInput {
            name: "flaky".to_string(),
            max_attempts: Some(1), // first failure is the last
            ..Default::default()
        },
    )
    .await;

    let mut w = worker(Arc::clone(&engine));
    w.register("flaky", Arc::new(Boom));
    w.process_next_job().await.unwrap();

    let stored = engine.get_job(id).await.unwrap().unwrap();
    assert_eq!(stored.status, MinionJobStatus::Dead);
    assert_eq!(stored.attempts_made, 1);
}

// --- behavior 11: promote-delayed -> claim closed loop ---------------------

/// Fails the first run, succeeds the second — proves the delayed job is
/// promoted back to waiting and re-claimed on a later turn.
struct FlipFlop(AtomicUsize);
#[async_trait]
impl MinionHandler for FlipFlop {
    async fn handle(&self, _ctx: &MinionJobContext) -> zbrain_core::Result<Value> {
        let n = self.0.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            Err(zbrain_core::error::StructuredError::new(
                "Handler", "handler", "first fails",
            ))
        } else {
            Ok(json!({"attempt": n + 1}))
        }
    }
}

#[tokio::test]
async fn due_delayed_job_is_promoted_then_claimed() {
    let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
    // 0ms backoff -> delayed job is immediately due for promotion next turn.
    let id = enqueue(
        &*engine,
        MinionJobInput {
            name: "eventual".to_string(),
            max_attempts: Some(3),
            backoff_type: Some(BackoffType::Fixed),
            backoff_delay: Some(0),
            backoff_jitter: Some(0.0),
            ..Default::default()
        },
    )
    .await;

    let mut w = worker(Arc::clone(&engine));
    w.register("eventual", Arc::new(FlipFlop(AtomicUsize::new(0))));

    // Turn 1: claim + fail -> delayed (0ms backoff).
    assert_eq!(
        w.process_next_job().await.unwrap(),
        ProcessOutcome::Processed(id)
    );
    assert_eq!(
        engine.get_job(id).await.unwrap().unwrap().status,
        MinionJobStatus::Delayed
    );

    // Turn 2: promote_delayed flips it back to waiting, then claim + succeed.
    assert_eq!(
        w.process_next_job().await.unwrap(),
        ProcessOutcome::Processed(id)
    );
    let stored = engine.get_job(id).await.unwrap().unwrap();
    assert_eq!(stored.status, MinionJobStatus::Completed);
    assert_eq!(stored.result, Some(json!({"attempt": 2})));
}
