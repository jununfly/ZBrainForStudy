//! Phase 9 slice 1-2-4: worker self-monitoring (RSS watchdog, health check).
//!
//! Tests the peripheral guardrails that can all be disabled via opts flags:
//! - `check_memory_limit` (injectable RSS reader)
//! - RSS periodic watchdog (short interval → shutdown)
//! - Health check timer (disabled/enabled, unhealthy channel wiring)

use std::sync::Arc;

use tokio::sync::mpsc;

use zbrain_core::engine::BrainEngine;
use zbrain_core::minions::types::MinionJobInput;
use zbrain_core::minions::{MinionHandler, MinionJobContext, MinionWorkerOpts};
use zbrain_core::InMemoryEngine;
use zbrain_worker::{MinionWorker, UnhealthyReason};

// --------------- helpers ---------------------------------------------------

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

// --------------- behavior 1: check_memory_limit exceeds threshold ----------

#[tokio::test]
async fn check_memory_limit_triggers_shutdown_when_exceeded() {
    let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
    let mut w = MinionWorker::new(
        Arc::clone(&engine),
        MinionWorkerOpts {
            max_rss_mb: Some(1), // 1MB threshold
            ..Default::default()
        },
    );
    // Inject RSS returning 5MB.
    w.set_rss_reader(|| 5 * 1024 * 1024);

    assert!(w.check_memory_limit());
    assert!(w.shutdown_token().is_cancelled());
}

#[tokio::test]
async fn check_memory_limit_noop_when_below_threshold() {
    let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
    let mut w = MinionWorker::new(
        Arc::clone(&engine),
        MinionWorkerOpts {
            max_rss_mb: Some(100), // 100MB threshold
            ..Default::default()
        },
    );
    w.set_rss_reader(|| 10 * 1024 * 1024); // 10MB < 100MB

    assert!(!w.check_memory_limit());
    assert!(!w.shutdown_token().is_cancelled());
}

// --------------- behavior 2: check_memory_limit disabled -------------------

#[tokio::test]
async fn check_memory_limit_noop_when_disabled() {
    let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
    let mut w = MinionWorker::new(
        Arc::clone(&engine),
        MinionWorkerOpts {
            max_rss_mb: None, // disabled
            ..Default::default()
        },
    );
    w.set_rss_reader(|| 999 * 1024 * 1024);

    assert!(!w.check_memory_limit());
}

#[tokio::test]
async fn check_memory_limit_noop_when_max_is_zero() {
    let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
    let mut w = MinionWorker::new(
        Arc::clone(&engine),
        MinionWorkerOpts {
            max_rss_mb: Some(0), // explicitly zero
            ..Default::default()
        },
    );
    w.set_rss_reader(|| 999 * 1024 * 1024);

    assert!(!w.check_memory_limit());
}

// --------------- behavior 3: RSS periodic watchdog timer -------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rss_watchdog_periodic_triggers_shutdown() {
    let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());

    let mut w = MinionWorker::new(
        Arc::clone(&engine),
        MinionWorkerOpts {
            concurrency: 1,
            poll_interval_ms: 100,
            max_rss_mb: Some(1),         // 1MB threshold
            rss_check_interval_ms: 1,    // check every 1ms
            health_check_interval_ms: 0, // disable health check
            ..Default::default()
        },
    );
    w.set_rss_reader(|| 10 * 1024 * 1024); // always 10MB > 1MB threshold

    let shutdown = w.shutdown_token();
    let handle = tokio::spawn(async move { w.run().await });

    // Wait for RSS watchdog to cancel shutdown token (should be very fast).
    tokio::time::timeout(std::time::Duration::from_secs(3), shutdown.cancelled())
        .await
        .expect("RSS watchdog should trigger shutdown within 3s");

    let _ = handle.await;
}

// --------------- behavior 4: health check disabled when interval=0 ---------

#[tokio::test]
async fn health_check_disabled_when_interval_zero() {
    let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
    enqueue(&*engine, job("work")).await;

    let (tx, mut rx) = mpsc::unbounded_channel::<UnhealthyReason>();
    let mut w = MinionWorker::new(
        Arc::clone(&engine),
        MinionWorkerOpts {
            concurrency: 1,
            poll_interval_ms: 10,
            health_check_interval_ms: 0, // disabled
            ..Default::default()
        },
    );
    w.on_unhealthy(tx);

    // Handler that completes quickly.
    struct Quick;
    #[async_trait::async_trait]
    impl MinionHandler for Quick {
        async fn handle(&self, _ctx: &MinionJobContext) -> zbrain_core::Result<serde_json::Value> {
            Ok(serde_json::json!("done"))
        }
    }
    w.register("work", Arc::new(Quick));

    let shutdown = w.shutdown_token();
    let handle = tokio::spawn(async move { w.run().await });

    // Wait for job to complete, then shutdown.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    shutdown.cancel();
    let _ = handle.await;

    // No unhealthy events should have been sent.
    assert!(rx.try_recv().is_err(), "no unhealthy events when health check disabled");
}

// --------------- behavior 5: on_unhealthy channel is wired -----------------

#[tokio::test]
async fn on_unhealthy_channel_is_wired() {
    let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
    let (tx, _rx) = mpsc::unbounded_channel::<UnhealthyReason>();

    let mut w = worker(Arc::clone(&engine));
    w.on_unhealthy(tx);
    // Channel is stored — the test is that it doesn't panic.
}

// --------------- behavior 6: check_memory_limit idempotent -----------------

#[tokio::test]
async fn check_memory_limit_is_idempotent() {
    let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
    let mut w = MinionWorker::new(
        Arc::clone(&engine),
        MinionWorkerOpts {
            max_rss_mb: Some(1),
            ..Default::default()
        },
    );
    w.set_rss_reader(|| 10 * 1024 * 1024);

    // First call triggers shutdown.
    assert!(w.check_memory_limit());
    // Second call: already shut down → return false (idempotent).
    assert!(!w.check_memory_limit());
}
