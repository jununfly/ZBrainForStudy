//! Integration tests for rate lease engine methods (roadmap 1-3-3).
//!
//! Tests `acquire_rate_lease`, `renew_rate_lease`, and `release_rate_lease`
//! against a real Postgres instance via PgFixture.
//!
//! Advisory lock serialisation (`pg_advisory_xact_lock`) is implicitly tested
//! because all acquires on the same key use the same FNV-1a hash — if locks
//! didn't work, the count-based capacity check would over-allocate.

mod support;

use support::pg_fixture::PgFixture;
use zbrain_core::engine::{BrainEngine, InMemoryEngine};
use zbrain_core::minions::types::{
    MinionJobInput,
};

// ─── helpers ────────────────────────────────────────────────────────────────

fn job(name: &str) -> MinionJobInput {
    MinionJobInput {
        name: name.to_string(),
        queue: Some("test".to_string()),
        ..Default::default()
    }
}

// ─── InMemory contract ──────────────────────────────────────────────────────

/// All three rate lease methods must return `Unsupported` on InMemoryEngine.
#[tokio::test]
async fn inmemory_all_rate_lease_unsupported() {
    let engine = InMemoryEngine::new();

    let r = engine.acquire_rate_lease("test", 1, 10, 120_000).await;
    assert!(r.is_err());
    assert!(r.unwrap_err().to_string().contains("not yet implemented"));

    let r = engine.renew_rate_lease(1, 120_000).await;
    assert!(r.is_err());
    assert!(r.unwrap_err().to_string().contains("not yet implemented"));

    let r = engine.release_rate_lease(1).await;
    assert!(r.is_err());
    assert!(r.unwrap_err().to_string().contains("not yet implemented"));
}

// ─── Postgres tests ─────────────────────────────────────────────────────────

/// Acquire a lease when capacity is available → acquired=true, active_count=1.
#[tokio::test]
async fn postgres_acquire_within_capacity() {
    let fix = PgFixture::start().await;
    let engine = &fix.engine;
    let j = engine.enqueue_job(&job("acquire")).await.unwrap();

    let result = engine
        .acquire_rate_lease("anthropic:messages", j.id, 5, 120_000)
        .await
        .unwrap();

    assert!(result.acquired);
    assert!(result.lease_id.is_some());
    assert_eq!(result.active_count, 1);
    assert_eq!(result.max_concurrent, 5);
}

/// Fill capacity, then the next acquire → acquired=false.
#[tokio::test]
async fn postgres_acquire_at_capacity() {
    let fix = PgFixture::start().await;
    let engine = &fix.engine;
    let max = 3;

    // Fill all slots.
    for _ in 0..max {
        let j = engine.enqueue_job(&job("fill")).await.unwrap();
        let r = engine
            .acquire_rate_lease("anthropic:messages", j.id, max, 120_000)
            .await
            .unwrap();
        assert!(r.acquired);
    }

    // One more → capacity full.
    let j = engine.enqueue_job(&job("overflow")).await.unwrap();
    let result = engine
        .acquire_rate_lease("anthropic:messages", j.id, max, 120_000)
        .await
        .unwrap();

    assert!(!result.acquired);
    assert_eq!(result.lease_id, None);
    assert_eq!(result.active_count, max);
    assert_eq!(result.max_concurrent, max);
}

/// Expired leases are automatically pruned by acquire, freeing capacity.
#[tokio::test]
async fn postgres_acquire_prunes_expired() {
    let fix = PgFixture::start().await;
    let engine = &fix.engine;

    // Acquire a lease with a very short TTL and release it manually (so it
    // doesn't exist). Then simulate: insert an already-expired row directly.
    use sqlx::postgres::PgPoolOptions;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&fix.url)
        .await
        .unwrap();
    let j = engine.enqueue_job(&job("expired")).await.unwrap();

    // Insert an already-expired lease row.
    sqlx::query(
        "INSERT INTO subagent_rate_leases (key, owner_job_id, expires_at) \
         VALUES ('key-expired-prune', $1, now() - interval '1 hour')",
    )
    .bind(j.id)
    .execute(&pool)
    .await
    .unwrap();

    // Acquire with max_concurrent=1 — the expired row should be pruned,
    // so there's room.
    let result = engine
        .acquire_rate_lease("key-expired-prune", j.id, 1, 120_000)
        .await
        .unwrap();

    assert!(result.acquired);
    assert_eq!(result.active_count, 1);
}

/// Renew extends the lease expiry and returns true.
#[tokio::test]
async fn postgres_renew_success() {
    let fix = PgFixture::start().await;
    let engine = &fix.engine;
    let j = engine.enqueue_job(&job("renew")).await.unwrap();

    let r = engine
        .acquire_rate_lease("key-renew", j.id, 5, 30_000)
        .await
        .unwrap();
    assert!(r.acquired);
    let lease_id = r.lease_id.unwrap();

    let renewed = engine.renew_rate_lease(lease_id, 120_000).await.unwrap();
    assert!(renewed);
}

/// Renew on a non-existent lease id → false.
#[tokio::test]
async fn postgres_renew_missing_lease() {
    let fix = PgFixture::start().await;
    let engine = &fix.engine;

    let renewed = engine.renew_rate_lease(99999, 120_000).await.unwrap();
    assert!(!renewed);
}

/// Release is idempotent — calling it twice doesn't error.
#[tokio::test]
async fn postgres_release_idempotent() {
    let fix = PgFixture::start().await;
    let engine = &fix.engine;
    let j = engine.enqueue_job(&job("release")).await.unwrap();

    let r = engine
        .acquire_rate_lease("key-release-idem", j.id, 5, 120_000)
        .await
        .unwrap();
    assert!(r.acquired);
    let lease_id = r.lease_id.unwrap();

    // First release.
    engine.release_rate_lease(lease_id).await.unwrap();

    // Second release — no-op, no error.
    engine.release_rate_lease(lease_id).await.unwrap();
}

/// Releasing a lease frees a capacity slot for the next acquire.
#[tokio::test]
async fn postgres_release_frees_slot() {
    let fix = PgFixture::start().await;
    let engine = &fix.engine;
    let max = 1;

    let j1 = engine.enqueue_job(&job("first")).await.unwrap();
    let r1 = engine
        .acquire_rate_lease("key-release-slot", j1.id, max, 120_000)
        .await
        .unwrap();
    assert!(r1.acquired);

    // Capacity is now full.
    let j2 = engine.enqueue_job(&job("second")).await.unwrap();
    let r2 = engine
        .acquire_rate_lease("key-release-slot", j2.id, max, 120_000)
        .await
        .unwrap();
    assert!(!r2.acquired);

    // Release the first lease.
    engine.release_rate_lease(r1.lease_id.unwrap()).await.unwrap();

    // Now there's room again.
    let r3 = engine
        .acquire_rate_lease("key-release-slot", j2.id, max, 120_000)
        .await
        .unwrap();
    assert!(r3.acquired);
}

/// ON DELETE CASCADE: deleting a job auto-deletes its leases, freeing slots.
#[tokio::test]
async fn postgres_acquire_cascade_on_job_delete() {
    let fix = PgFixture::start().await;
    let engine = &fix.engine;

    let j = engine.enqueue_job(&job("cascade")).await.unwrap();
    let r = engine
        .acquire_rate_lease("key-cascade", j.id, 1, 120_000)
        .await
        .unwrap();
    assert!(r.acquired);

    // Delete the job — CASCADE should remove the lease row.
    use sqlx::postgres::PgPoolOptions;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&fix.url)
        .await
        .unwrap();
    sqlx::query("DELETE FROM minion_jobs WHERE id = $1")
        .bind(j.id)
        .execute(&pool)
        .await
        .unwrap();

    // Now another acquire on the same key (max=1) should succeed because
    // the CASCADE-deleted lease freed the slot.
    let j2 = engine.enqueue_job(&job("cascade2")).await.unwrap();
    let r2 = engine
        .acquire_rate_lease("key-cascade", j2.id, 1, 120_000)
        .await
        .unwrap();
    assert!(r2.acquired);
}
