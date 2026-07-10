//! Phase 9 (slice 1-1-1 A+B): Minion job queue integration tests.
//!
//! Exercises the backend-agnostic queue contract (enqueue/get/claim/complete/
//! fail/retry/renew) against all three backends. Backend-agnostic `contract_*`
//! functions run once per backend so InMemory, Libsql, and Postgres are held to
//! the same behavior. The `minion_jobs` table has no FK to `sources` and A+B
//! never sets `parent_job_id`, so no seeding is required.

mod support;

use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::minions::types::{
    FailOutcome, JobFilters, MinionJobInput, MinionJobStatus,
};
use zbrain_core::InMemoryEngine;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn job(name: &str) -> MinionJobInput {
    MinionJobInput {
        name: name.to_string(),
        ..Default::default()
    }
}

async fn init_clean_libsql() -> (LibsqlEngine, NamedTempFile) {
    let path = NamedTempFile::new().expect("alloc temp db file");
    let engine = LibsqlEngine::new();
    let cfg = EngineConfig {
        database_url: None,
        database_path: Some(path.path().to_string_lossy().into_owned()),
    };
    engine.connect(&cfg).await.expect("connect");
    engine.init_schema().await.expect("init_schema");
    (engine, path)
}

// ---------------------------------------------------------------------------
// Backend-agnostic contract functions
// ---------------------------------------------------------------------------

async fn contract_enqueue_defaults(engine: &dyn BrainEngine) {
    let created = engine.enqueue_job(&job("build")).await.unwrap();
    assert_eq!(created.name, "build");
    assert_eq!(created.queue, "default");
    assert_eq!(created.status, MinionJobStatus::Waiting);
    assert_eq!(created.max_attempts, 3);
    assert_eq!(created.max_stalled, 5);
    assert_eq!(created.backoff_delay, 1000);
    assert!((created.backoff_jitter - 0.2).abs() < 1e-9);
    assert!(created.lock_token.is_none());

    let fetched = engine.get_job(created.id).await.unwrap().unwrap();
    assert_eq!(fetched.id, created.id);
    assert_eq!(fetched.name, "build");
}

async fn contract_idempotency(engine: &dyn BrainEngine) {
    let input = MinionJobInput {
        idempotency_key: Some("k1".to_string()),
        ..job("once")
    };
    let a = engine.enqueue_job(&input).await.unwrap();
    let b = engine.enqueue_job(&input).await.unwrap();
    assert_eq!(a.id, b.id, "same key returns the same row");

    let all = engine
        .get_jobs(&JobFilters {
            name: Some("once".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(all.len(), 1, "no second row inserted");
}

async fn contract_delay_sets_delayed(engine: &dyn BrainEngine) {
    let before = zbrain_core::time::now_epoch_ms();
    let j = engine
        .enqueue_job(&MinionJobInput {
            delay: Some(60_000),
            ..job("later")
        })
        .await
        .unwrap();
    assert_eq!(j.status, MinionJobStatus::Delayed);
    assert!(j.delay_until.unwrap() >= before + 60_000);
}

async fn contract_claim_priority_and_exclusive(engine: &dyn BrainEngine) {
    engine
        .enqueue_job(&MinionJobInput {
            priority: Some(5),
            ..job("worker")
        })
        .await
        .unwrap();
    let hot = engine
        .enqueue_job(&MinionJobInput {
            priority: Some(0),
            ..job("worker")
        })
        .await
        .unwrap();

    let names = vec!["worker".to_string()];
    let first = engine
        .claim_job("tok-1", 30_000, "default", &names)
        .await
        .unwrap()
        .expect("claimable");
    assert_eq!(first.id, hot.id, "priority 0 before priority 5");
    assert_eq!(first.status, MinionJobStatus::Active);
    assert_eq!(first.lock_token.as_deref(), Some("tok-1"));
    assert_eq!(first.attempts_started, 1);
    assert!(first.started_at.is_some());
    assert!(first.lock_until.is_some());

    let second = engine
        .claim_job("tok-2", 30_000, "default", &names)
        .await
        .unwrap()
        .expect("second waiting job");
    assert_ne!(second.id, first.id);

    assert!(engine
        .claim_job("tok-3", 30_000, "default", &names)
        .await
        .unwrap()
        .is_none());
}

async fn contract_claim_filters(engine: &dyn BrainEngine) {
    engine
        .enqueue_job(&MinionJobInput {
            queue: Some("shell".to_string()),
            ..job("run")
        })
        .await
        .unwrap();

    // Wrong queue / unregistered name / empty names all claim nothing.
    assert!(engine
        .claim_job("t", 1000, "default", &["run".to_string()])
        .await
        .unwrap()
        .is_none());
    assert!(engine
        .claim_job("t", 1000, "shell", &["other".to_string()])
        .await
        .unwrap()
        .is_none());
    assert!(engine
        .claim_job("t", 1000, "shell", &[])
        .await
        .unwrap()
        .is_none());
    // Correct queue + name succeeds.
    assert!(engine
        .claim_job("t", 1000, "shell", &["run".to_string()])
        .await
        .unwrap()
        .is_some());
}

async fn contract_complete_token_fence(engine: &dyn BrainEngine) {
    engine.enqueue_job(&job("w")).await.unwrap();
    let names = vec!["w".to_string()];
    let claimed = engine
        .claim_job("good", 30_000, "default", &names)
        .await
        .unwrap()
        .unwrap();

    // Wrong token -> None, still active.
    assert!(engine
        .complete_job(claimed.id, "bad", None)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        engine.get_job(claimed.id).await.unwrap().unwrap().status,
        MinionJobStatus::Active
    );

    // Right token -> completed with result.
    let done = engine
        .complete_job(claimed.id, "good", Some(&serde_json::json!({"ok": true})))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.status, MinionJobStatus::Completed);
    assert_eq!(done.result, Some(serde_json::json!({"ok": true})));
    assert!(done.finished_at.is_some());
    assert!(done.lock_token.is_none());
}

async fn contract_fail_delayed_then_retry(engine: &dyn BrainEngine) {
    engine.enqueue_job(&job("w")).await.unwrap();
    let names = vec!["w".to_string()];
    let claimed = engine
        .claim_job("tok", 30_000, "default", &names)
        .await
        .unwrap()
        .unwrap();

    let before = zbrain_core::time::now_epoch_ms();
    let failed = engine
        .fail_job(claimed.id, "tok", "boom", FailOutcome::Delayed, 5_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed.status, MinionJobStatus::Delayed);
    assert_eq!(failed.attempts_made, 1);
    assert_eq!(failed.error_text.as_deref(), Some("boom"));
    assert_eq!(failed.stacktrace, vec!["boom".to_string()]);
    assert!(failed.finished_at.is_none());
    assert!(failed.delay_until.unwrap() >= before + 5_000);
}

async fn contract_fail_terminal_then_retry(engine: &dyn BrainEngine) {
    engine.enqueue_job(&job("w")).await.unwrap();
    let names = vec!["w".to_string()];
    let claimed = engine
        .claim_job("tok", 30_000, "default", &names)
        .await
        .unwrap()
        .unwrap();
    let failed = engine
        .fail_job(claimed.id, "tok", "nope", FailOutcome::Failed, 0)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed.status, MinionJobStatus::Failed);
    assert!(failed.finished_at.is_some());

    let requeued = engine
        .retry_job(claimed.id)
        .await
        .unwrap()
        .expect("failed job is retryable");
    assert_eq!(requeued.status, MinionJobStatus::Waiting);
    assert!(requeued.error_text.is_none());
    assert!(requeued.finished_at.is_none());
    assert!(requeued.delay_until.is_none());
    assert!(requeued.lock_token.is_none());

    // A waiting job is not retryable.
    assert!(engine.retry_job(claimed.id).await.unwrap().is_none());
}

async fn contract_renew_lock(engine: &dyn BrainEngine) {
    engine.enqueue_job(&job("w")).await.unwrap();
    let names = vec!["w".to_string()];
    let claimed = engine
        .claim_job("tok", 1_000, "default", &names)
        .await
        .unwrap()
        .unwrap();

    assert!(!engine
        .renew_job_lock(claimed.id, "bad", 30_000)
        .await
        .unwrap());
    assert!(engine
        .renew_job_lock(claimed.id, "tok", 30_000)
        .await
        .unwrap());
    let renewed = engine.get_job(claimed.id).await.unwrap().unwrap();
    assert!(renewed.lock_until.unwrap() >= zbrain_core::time::now_epoch_ms() + 29_000);
}

// ---------------------------------------------------------------------------
// Background sweeps (1-1-2). Time-driven state machines tested WITHOUT sleeping
// (roadmap 1-1-2 decision 6): the scheduling columns are epoch-ms integers, so
// a job made eligible with a zero/negative delay or a negative lock duration is
// already "in the past" the moment it is written; `started_at` is forced into
// the past via `set_started_at_for_test`. No injectable clock — the sweeps read
// wall-clock `now` in SQL exactly as production does.
// ---------------------------------------------------------------------------

/// A delayed job whose `delay_until` is already <= now is promoted to waiting,
/// with delay/lock fields cleared. A future-delayed job is left untouched.
async fn contract_promote_delayed(engine: &dyn BrainEngine) {
    // backoff_ms = 0 => delay_until = now, so `delay_until <= now` holds.
    engine.enqueue_job(&job("due")).await.unwrap();
    let names = vec!["due".to_string()];
    let claimed = engine
        .claim_job("tok", 30_000, "default", &names)
        .await
        .unwrap()
        .unwrap();
    let delayed = engine
        .fail_job(claimed.id, "tok", "retry", FailOutcome::Delayed, 0)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delayed.status, MinionJobStatus::Delayed);

    // A second job delayed far into the future must NOT be promoted.
    let future = engine
        .enqueue_job(&MinionJobInput {
            delay: Some(3_600_000),
            ..job("future")
        })
        .await
        .unwrap();
    assert_eq!(future.status, MinionJobStatus::Delayed);

    let promoted = engine.promote_delayed().await.unwrap();
    assert_eq!(promoted.len(), 1, "only the due job is promoted");
    let p = &promoted[0];
    assert_eq!(p.id, delayed.id);
    assert_eq!(p.status, MinionJobStatus::Waiting);
    assert!(p.delay_until.is_none());
    assert!(p.lock_token.is_none());
    assert!(p.lock_until.is_none());

    // The future job is still delayed.
    assert_eq!(
        engine.get_job(future.id).await.unwrap().unwrap().status,
        MinionJobStatus::Delayed
    );
}

/// A stalled active job (lease expired) under its stall budget is requeued to
/// waiting with `stalled_counter` bumped; at/over budget it is dead-lettered.
async fn contract_handle_stalled(engine: &dyn BrainEngine) {
    // Two jobs. Claim each with a NEGATIVE lock duration so lock_until < now
    // immediately (the lease is "already expired").
    engine.enqueue_job(&job("s")).await.unwrap();
    engine
        .enqueue_job(&MinionJobInput {
            // max_stalled = 1 => stalled_counter(0) + 1 >= 1 => dead-lettered.
            max_stalled: Some(1),
            ..job("s")
        })
        .await
        .unwrap();
    let names = vec!["s".to_string()];

    let requeue_target = engine
        .claim_job("t1", -1, "default", &names)
        .await
        .unwrap()
        .unwrap();
    let dead_target = engine
        .claim_job("t2", -1, "default", &names)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(requeue_target.max_stalled, 5);
    assert_eq!(dead_target.max_stalled, 1);

    let sweep = engine.handle_stalled().await.unwrap();
    assert_eq!(sweep.requeued.len(), 1, "one under-budget job requeued");
    assert_eq!(sweep.dead.len(), 1, "one at-budget job dead-lettered");

    let requeued = &sweep.requeued[0];
    assert_eq!(requeued.id, requeue_target.id);
    assert_eq!(requeued.status, MinionJobStatus::Waiting);
    assert_eq!(requeued.stalled_counter, 1);
    assert!(requeued.lock_token.is_none());
    assert!(requeued.lock_until.is_none());

    let dead = &sweep.dead[0];
    assert_eq!(dead.id, dead_target.id);
    assert_eq!(dead.status, MinionJobStatus::Dead);
    assert_eq!(dead.stalled_counter, 1);
    assert_eq!(dead.error_text.as_deref(), Some("max stalled count exceeded"));
    assert!(dead.finished_at.is_some());

    // Idempotent: nothing active-and-stalled remains.
    let again = engine.handle_stalled().await.unwrap();
    assert!(again.requeued.is_empty() && again.dead.is_empty());
}

/// An active job whose per-job `timeout_at` has passed while the lease is still
/// held is dead-lettered. A stalled job (lease expired) is left for
/// `handle_stalled`, not timed out here.
async fn contract_handle_timeouts(engine: &dyn BrainEngine) {
    // A positive timeout_ms satisfies the chk_minion_timeout_positive CHECK on
    // the SQL backends; we then force timeout_at into the past via the test
    // helper. Positive lock duration keeps lock_until > now (not stalled).
    engine
        .enqueue_job(&MinionJobInput {
            timeout_ms: Some(30_000),
            ..job("t")
        })
        .await
        .unwrap();
    let names = vec!["t".to_string()];
    let claimed = engine
        .claim_job("tok", 30_000, "default", &names)
        .await
        .unwrap()
        .unwrap();
    engine
        .set_timeout_at_for_test(claimed.id, zbrain_core::time::now_epoch_ms() - 1)
        .await
        .unwrap();

    // A second job with no timeout_ms must be untouched.
    engine.enqueue_job(&job("t")).await.unwrap();
    let safe = engine
        .claim_job("tok2", 30_000, "default", &names)
        .await
        .unwrap()
        .unwrap();

    let timed_out = engine.handle_timeouts().await.unwrap();
    assert_eq!(timed_out.len(), 1, "only the expired-timeout job dies");
    let t = &timed_out[0];
    assert_eq!(t.id, claimed.id);
    assert_eq!(t.status, MinionJobStatus::Dead);
    assert_eq!(t.error_text.as_deref(), Some("timeout exceeded"));
    assert!(t.finished_at.is_some());
    assert!(t.lock_token.is_none());

    assert_eq!(
        engine.get_job(safe.id).await.unwrap().unwrap().status,
        MinionJobStatus::Active
    );

    // A stalled job (expired lease) is NOT swept by handle_timeouts. Give it a
    // past timeout_at too, so only the lease-held guard keeps it out.
    engine.enqueue_job(&job("t")).await.unwrap();
    let stalled = engine
        .claim_job("tok3", -1, "default", &names)
        .await
        .unwrap()
        .unwrap();
    engine
        .set_timeout_at_for_test(stalled.id, zbrain_core::time::now_epoch_ms() - 1)
        .await
        .unwrap();
    let none = engine.handle_timeouts().await.unwrap();
    assert!(
        none.iter().all(|j| j.id != stalled.id),
        "stalled job left for handle_stalled, not timed out"
    );
}

/// An active job whose wall-clock runtime exceeds the threshold is dead-lettered
/// regardless of lease state. We force `started_at` far into the past via the
/// test-only helper so the SQL `now() - started_at` arithmetic trips.
async fn contract_handle_wall_clock_timeouts(engine: &dyn BrainEngine) {
    // No timeout_ms => threshold = lock_duration_ms * 2 * GREATEST(max_stalled, 1).
    // With lock_duration_ms = 1000 and max_stalled = 5 (default) => 10_000 ms.
    engine.enqueue_job(&job("wc")).await.unwrap();
    let names = vec!["wc".to_string()];
    let claimed = engine
        .claim_job("tok", 60_000, "default", &names)
        .await
        .unwrap()
        .unwrap();

    // Force started_at to 10 minutes ago (well past any threshold).
    let past = (chrono::Utc::now() - chrono::Duration::minutes(10)).to_rfc3339();
    engine
        .set_started_at_for_test(claimed.id, &past)
        .await
        .unwrap();

    // A second freshly-started job must survive the same sweep.
    engine.enqueue_job(&job("wc")).await.unwrap();
    let fresh = engine
        .claim_job("tok2", 60_000, "default", &names)
        .await
        .unwrap()
        .unwrap();

    let dead = engine.handle_wall_clock_timeouts(1_000).await.unwrap();
    assert_eq!(dead.len(), 1, "only the long-running job dies");
    let d = &dead[0];
    assert_eq!(d.id, claimed.id);
    assert_eq!(d.status, MinionJobStatus::Dead);
    assert_eq!(
        d.error_text.as_deref(),
        Some("wall-clock timeout exceeded")
    );
    assert!(d.finished_at.is_some());
    assert!(d.lock_token.is_none());

    assert_eq!(
        engine.get_job(fresh.id).await.unwrap().unwrap().status,
        MinionJobStatus::Active
    );
}

// ---------------------------------------------------------------------------
// InMemory
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inmemory_enqueue_defaults() {
    contract_enqueue_defaults(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_idempotency() {
    contract_idempotency(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_delay() {
    contract_delay_sets_delayed(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_claim_priority() {
    contract_claim_priority_and_exclusive(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_claim_filters() {
    contract_claim_filters(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_complete_token_fence() {
    contract_complete_token_fence(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_fail_delayed() {
    contract_fail_delayed_then_retry(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_fail_terminal_retry() {
    contract_fail_terminal_then_retry(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_renew_lock() {
    contract_renew_lock(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_promote_delayed() {
    contract_promote_delayed(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_handle_stalled() {
    contract_handle_stalled(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_handle_timeouts() {
    contract_handle_timeouts(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_handle_wall_clock_timeouts() {
    contract_handle_wall_clock_timeouts(&InMemoryEngine::new()).await;
}

// ---------------------------------------------------------------------------
// Libsql
// ---------------------------------------------------------------------------

#[tokio::test]
async fn libsql_enqueue_defaults() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_enqueue_defaults(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_idempotency() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_idempotency(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_delay() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_delay_sets_delayed(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_claim_priority() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_claim_priority_and_exclusive(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_claim_filters() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_claim_filters(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_complete_token_fence() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_complete_token_fence(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_fail_delayed() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_fail_delayed_then_retry(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_fail_terminal_retry() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_fail_terminal_then_retry(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_renew_lock() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_renew_lock(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_promote_delayed() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_promote_delayed(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_handle_stalled() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_handle_stalled(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_handle_timeouts() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_handle_timeouts(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_handle_wall_clock_timeouts() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_handle_wall_clock_timeouts(&engine).await;
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_job_survives_reconnect() {
    let path = NamedTempFile::new().expect("alloc temp db file");
    let cfg = EngineConfig {
        database_url: None,
        database_path: Some(path.path().to_string_lossy().into_owned()),
    };

    let id = {
        let engine = LibsqlEngine::new();
        engine.connect(&cfg).await.expect("connect");
        engine.init_schema().await.expect("init_schema");
        let j = engine.enqueue_job(&job("persist")).await.unwrap();
        engine.disconnect().await.expect("disconnect");
        j.id
    };

    let engine2 = LibsqlEngine::new();
    engine2.connect(&cfg).await.expect("reconnect");
    engine2.init_schema().await.expect("reinit schema");
    let fetched = engine2.get_job(id).await.unwrap().expect("row persisted");
    assert_eq!(fetched.name, "persist");
    engine2.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// Postgres
// ---------------------------------------------------------------------------

#[tokio::test]
async fn postgres_enqueue_defaults() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_enqueue_defaults(&fix.engine).await;
}
#[tokio::test]
async fn postgres_idempotency() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_idempotency(&fix.engine).await;
}
#[tokio::test]
async fn postgres_delay() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_delay_sets_delayed(&fix.engine).await;
}
#[tokio::test]
async fn postgres_claim_priority() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_claim_priority_and_exclusive(&fix.engine).await;
}
#[tokio::test]
async fn postgres_claim_filters() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_claim_filters(&fix.engine).await;
}
#[tokio::test]
async fn postgres_complete_token_fence() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_complete_token_fence(&fix.engine).await;
}
#[tokio::test]
async fn postgres_fail_delayed() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_fail_delayed_then_retry(&fix.engine).await;
}
#[tokio::test]
async fn postgres_fail_terminal_retry() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_fail_terminal_then_retry(&fix.engine).await;
}
#[tokio::test]
async fn postgres_renew_lock() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_renew_lock(&fix.engine).await;
}
#[tokio::test]
async fn postgres_promote_delayed() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_promote_delayed(&fix.engine).await;
}
#[tokio::test]
async fn postgres_handle_stalled() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_handle_stalled(&fix.engine).await;
}
#[tokio::test]
async fn postgres_handle_timeouts() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_handle_timeouts(&fix.engine).await;
}
#[tokio::test]
async fn postgres_handle_wall_clock_timeouts() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_handle_wall_clock_timeouts(&fix.engine).await;
}
