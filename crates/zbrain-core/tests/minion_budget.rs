//! Minion budget management integration tests (roadmap 1-3-2).
//!
//! Exercises the 6 budget methods on the `BrainEngine` trait against
//! Postgres (the only backend with real budget implementation) and
//! verifies that InMemory returns Unsupported for all methods.
//!
//! ## Contract coverage
//!
//! | Contract                              | reserve | refund | set_owner | halt | inherit | get_owner | log |
//! |---------------------------------------|---------|--------|-----------|------|---------|-----------|-----|
//! | `set_and_reserve_within_budget`        | x       |        | x         |      |         |           |     |
//! | `reserve_no_budget`                   | x       |        |           |      |         |           |     |
//! | `reserve_exhausted`                   | x       |        | x         |      |         |           |     |
//! | `reserve_cas_atomicity`               | x       |        | x         |      |         |           |     |
//! | `refund_restores_budget`              | x       | x      | x         |      |         |           | x   |
//! | `halt_budget_subtree`                 |         |        | x         | x    |         |           |     |
//! | `inherit_budget_owner`               |         |        |           |      | x       | x         |     |
//! | `budget_log_audit_trail`             | x       | x      | x         |      |         |           | x   |
//! | `reserve_owner_deleted` (PG-only)    | x       |        | x         |      |         |           |     |
//!
//! InMemory: all 6 methods return `Error::Unsupported`.

mod support;

use support::pg_fixture::PgFixture;
use zbrain_core::engine::BrainEngine;
use zbrain_core::minions::types::{MinionJobInput, ReservationOutcome};
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

// ---------------------------------------------------------------------------
// Backend-agnostic contract functions (for Postgres)
// ---------------------------------------------------------------------------

/// Set a budget on a job, then reserve within it — should return Reserved.
async fn contract_set_and_reserve_within_budget(engine: &dyn BrainEngine) {
    let j = engine.enqueue_job(&job("budgeted")).await.unwrap();

    engine.set_owner_budget(j.id, 1000).await.unwrap();

    let outcome = engine.reserve_budget(j.id, 300, "test-reserve").await.unwrap();
    assert_eq!(outcome, ReservationOutcome::Reserved);

    // Second reserve within remaining (700) should also succeed.
    let outcome2 = engine.reserve_budget(j.id, 500, "test-reserve-2").await.unwrap();
    assert_eq!(outcome2, ReservationOutcome::Reserved);
}

/// Reserve on a job that has never had a budget → NoBudget.
async fn contract_reserve_no_budget(engine: &dyn BrainEngine) {
    let j = engine.enqueue_job(&job("no-budget")).await.unwrap();

    let outcome = engine.reserve_budget(j.id, 100, "test").await.unwrap();
    assert_eq!(outcome, ReservationOutcome::NoBudget);
}

/// Set a budget, then reserve more than remaining → Exhausted.
async fn contract_reserve_exhausted(engine: &dyn BrainEngine) {
    let j = engine.enqueue_job(&job("exhausted")).await.unwrap();

    engine.set_owner_budget(j.id, 500).await.unwrap();

    // Reserve more than the entire budget.
    let outcome = engine.reserve_budget(j.id, 600, "overspend").await.unwrap();
    assert_eq!(outcome, ReservationOutcome::Exhausted);
}

/// Reserve the exact remaining amount succeeds (balance→0), then the
/// next reserve on the empty balance gets Exhausted via CAS WHERE clause.
/// This verifies the CAS atomicity guard.
async fn contract_reserve_cas_atomicity(engine: &dyn BrainEngine) {
    let j = engine.enqueue_job(&job("cas")).await.unwrap();

    engine.set_owner_budget(j.id, 1000).await.unwrap();

    // Exact drain: reserve all 1000 succeeds (remaining >= amount).
    let outcome = engine.reserve_budget(j.id, 1000, "drain").await.unwrap();
    assert_eq!(outcome, ReservationOutcome::Reserved);

    // Next reserve on drained budget → CAS WHERE fails → Exhausted.
    let outcome2 = engine.reserve_budget(j.id, 1, "after-drain").await.unwrap();
    assert_eq!(outcome2, ReservationOutcome::Exhausted);
}

/// Reserve, then refund — balance should be restored.
async fn contract_refund_restores_budget(engine: &dyn BrainEngine) {
    let j = engine.enqueue_job(&job("refund")).await.unwrap();

    engine.set_owner_budget(j.id, 1000).await.unwrap();

    // Reserve 400.
    let outcome = engine.reserve_budget(j.id, 400, "spend").await.unwrap();
    assert_eq!(outcome, ReservationOutcome::Reserved);

    // Refund 200.
    engine.refund_budget(j.id, 200, "partial-refund").await.unwrap();

    // Should have 800 remaining (1000 - 400 + 200). Exact drain succeeds.
    let outcome2 = engine.reserve_budget(j.id, 800, "drain").await.unwrap();
    assert_eq!(outcome2, ReservationOutcome::Reserved);
}

/// Set budget on multiple jobs sharing the same owner, halt the subtree,
/// then verify all budgets became NULL.
async fn contract_halt_budget_subtree(engine: &dyn BrainEngine) {
    let owner = engine.enqueue_job(&job("owner")).await.unwrap();
    let _child1 = engine.enqueue_job(&job("child1")).await.unwrap();
    let _child2 = engine.enqueue_job(&job("child2")).await.unwrap();

    // Set the owner's own budget and make it the budget owner for children.
    engine.set_owner_budget(owner.id, 1000).await.unwrap();

    // Direct SQL needed to set budget_owner_job_id on children.
    // For contract functions we use set_owner_budget on child1/child2 with
    // the owner's id. But set_owner_budget(self, job_id, budget_cents) sets
    // budget_owner_job_id = job_id (the second param). It's designed for
    // self-owned jobs. So we need a PG-only test for this scenario.
    //
    // For the contract: halt on a self-owned job should clear its own budget.
    let affected = engine.halt_budget_subtree(owner.id).await.unwrap();
    assert_eq!(affected, 1, "halt should affect the owner job itself");

    // Owner's budget should now be NULL.
    let outcome = engine.reserve_budget(owner.id, 1, "after-halt").await.unwrap();
    assert_eq!(outcome, ReservationOutcome::NoBudget);
}

/// Change budget ownership for a job.
async fn contract_inherit_budget_owner(engine: &dyn BrainEngine) {
    let j = engine.enqueue_job(&job("inherit")).await.unwrap();
    let new_owner = engine.enqueue_job(&job("new-owner")).await.unwrap();

    // Set initial budget with j as its own owner.
    engine.set_owner_budget(j.id, 500).await.unwrap();

    // Inherit to new_owner.
    engine.inherit_budget_owner(j.id, new_owner.id).await.unwrap();

    // get_budget_owner should now return new_owner.
    let owner = engine.get_budget_owner(j.id).await.unwrap();
    assert_eq!(owner, Some(new_owner.id));
}

/// get_budget_owner returns None for jobs without a budget.
async fn contract_get_budget_owner_none(engine: &dyn BrainEngine) {
    let j = engine.enqueue_job(&job("no-owner")).await.unwrap();

    let owner = engine.get_budget_owner(j.id).await.unwrap();
    assert_eq!(owner, None);
}

/// Verify audit log entries are created for reserve and refund.
async fn contract_budget_log_audit_trail(engine: &dyn BrainEngine, url: &str) {
    use sqlx::postgres::PgPoolOptions;
    use sqlx::Row;

    let j = engine.enqueue_job(&job("audited")).await.unwrap();
    engine.set_owner_budget(j.id, 2000).await.unwrap();

    // Perform a reserve and a refund.
    engine.reserve_budget(j.id, 700, "audit-reserve").await.unwrap();
    engine.refund_budget(j.id, 200, "audit-refund").await.unwrap();

    // Verify log entries via direct SQL.
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(url)
        .await
        .unwrap();

    let rows = sqlx::query(
        "SELECT cents_delta, reason FROM minion_budget_log WHERE job_id = $1 ORDER BY id",
    )
    .bind(j.id)
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(rows.len(), 2, "should have 2 log entries");

    // cents_delta is INTEGER (INT4) in PG; read as i32.
    let delta1: i32 = rows[0].get("cents_delta");
    let reason1: String = rows[0].get("reason");
    assert_eq!(delta1, 700);
    assert_eq!(reason1, "audit-reserve");

    let delta2: i32 = rows[1].get("cents_delta");
    let reason2: String = rows[1].get("reason");
    assert_eq!(delta2, -200);
    assert_eq!(reason2, "audit-refund");

    pool.close().await;
}

// ---------------------------------------------------------------------------
// InMemory: all 6 methods return Unsupported
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inmemory_all_budget_unsupported() {
    let engine = InMemoryEngine::new();
    let j = engine.enqueue_job(&job("test")).await.unwrap();

    assert!(engine.reserve_budget(j.id, 100, "test").await.is_err());
    assert!(engine.refund_budget(j.id, 100, "test").await.is_err());
    assert!(engine.set_owner_budget(j.id, 1000).await.is_err());
    assert!(engine.halt_budget_subtree(j.id).await.is_err());
    assert!(engine.inherit_budget_owner(j.id, j.id).await.is_err());
    assert!(engine.get_budget_owner(j.id).await.is_err());
}

// ---------------------------------------------------------------------------
// Postgres: contract functions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn postgres_set_and_reserve_within_budget() {
    let fix = PgFixture::start().await;
    contract_set_and_reserve_within_budget(&fix.engine).await;
}

#[tokio::test]
async fn postgres_reserve_no_budget() {
    let fix = PgFixture::start().await;
    contract_reserve_no_budget(&fix.engine).await;
}

#[tokio::test]
async fn postgres_reserve_exhausted() {
    let fix = PgFixture::start().await;
    contract_reserve_exhausted(&fix.engine).await;
}

#[tokio::test]
async fn postgres_reserve_cas_atomicity() {
    let fix = PgFixture::start().await;
    contract_reserve_cas_atomicity(&fix.engine).await;
}

#[tokio::test]
async fn postgres_refund_restores_budget() {
    let fix = PgFixture::start().await;
    contract_refund_restores_budget(&fix.engine).await;
}

#[tokio::test]
async fn postgres_halt_budget_subtree() {
    let fix = PgFixture::start().await;
    contract_halt_budget_subtree(&fix.engine).await;
}

#[tokio::test]
async fn postgres_inherit_budget_owner() {
    let fix = PgFixture::start().await;
    contract_inherit_budget_owner(&fix.engine).await;
}

#[tokio::test]
async fn postgres_get_budget_owner_none() {
    let fix = PgFixture::start().await;
    contract_get_budget_owner_none(&fix.engine).await;
}

#[tokio::test]
async fn postgres_budget_log_audit_trail() {
    let fix = PgFixture::start().await;
    contract_budget_log_audit_trail(&fix.engine, &fix.url).await;
}

// ---------------------------------------------------------------------------
// Postgres-only tests (require direct SQL setup)
// ---------------------------------------------------------------------------

/// When the budget owner job is deleted, ON DELETE SET NULL triggers,
/// and reserve_budget should return OwnerDeleted.
#[tokio::test]
async fn postgres_reserve_owner_deleted() {
    use sqlx::postgres::PgPoolOptions;

    let fix = PgFixture::start().await;
    let engine = &fix.engine;

    let owner = engine.enqueue_job(&job("owner")).await.unwrap();
    let child = engine.enqueue_job(&job("child")).await.unwrap();

    // Set up: owner has a budget, child has budget_remaining_cents but
    // budget_owner_job_id points to owner.
    engine.set_owner_budget(owner.id, 1000).await.unwrap();

    // Manually wire child's budget to point to owner via direct SQL.
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&fix.url)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE minion_jobs SET budget_remaining_cents = 500, budget_owner_job_id = $1 WHERE id = $2",
    )
    .bind(owner.id)
    .bind(child.id)
    .execute(&pool)
    .await
    .unwrap();

    // Verify child can reserve through the owner.
    let outcome = engine.reserve_budget(child.id, 200, "before-delete").await.unwrap();
    assert_eq!(outcome, ReservationOutcome::Reserved);

    // Delete the owner job (cascades: minion_budget_log rows, sets child FK to NULL).
    sqlx::query("DELETE FROM minion_jobs WHERE id = $1")
        .bind(owner.id)
        .execute(&pool)
        .await
        .unwrap();

    // Child should now see OwnerDeleted because its budget_owner_job_id became NULL.
    let outcome = engine.reserve_budget(child.id, 1, "after-delete").await.unwrap();
    assert_eq!(outcome, ReservationOutcome::OwnerDeleted);

    pool.close().await;
}

/// halt_budget_subtree affects all jobs whose budget_owner_job_id matches,
/// not just the owner itself.
#[tokio::test]
async fn postgres_halt_budget_subtree_multi_child() {
    use sqlx::postgres::PgPoolOptions;

    let fix = PgFixture::start().await;
    let engine = &fix.engine;

    let owner = engine.enqueue_job(&job("owner")).await.unwrap();
    let child1 = engine.enqueue_job(&job("child1")).await.unwrap();
    let child2 = engine.enqueue_job(&job("child2")).await.unwrap();
    let unrelated = engine.enqueue_job(&job("unrelated")).await.unwrap();

    // Set budgets on all three jobs: owner self-owned, children point to owner.
    engine.set_owner_budget(owner.id, 2000).await.unwrap();

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&fix.url)
        .await
        .unwrap();

    for child_id in &[child1.id, child2.id] {
        sqlx::query(
            "UPDATE minion_jobs SET budget_remaining_cents = 800, budget_owner_job_id = $1 WHERE id = $2",
        )
        .bind(owner.id)
        .bind(child_id)
        .execute(&pool)
        .await
        .unwrap();
    }

    // Unrelated job: self-owned, different owner.
    engine.set_owner_budget(unrelated.id, 500).await.unwrap();

    // Halt the owner's subtree.
    let affected = engine.halt_budget_subtree(owner.id).await.unwrap();
    assert_eq!(affected, 3, "should affect owner + child1 + child2");

    // All three should now be NoBudget.
    for job_id in &[owner.id, child1.id, child2.id] {
        let outcome = engine.reserve_budget(*job_id, 1, "after-halt").await.unwrap();
        assert_eq!(
            outcome,
            ReservationOutcome::NoBudget,
            "job {job_id} should be NoBudget after halt"
        );
    }

    // Unrelated should still have its budget.
    let outcome = engine.reserve_budget(unrelated.id, 1, "unrelated").await.unwrap();
    assert_eq!(outcome, ReservationOutcome::Reserved);

    pool.close().await;
}

/// set_owner_budget on the same job twice overwrites previous value.
#[tokio::test]
async fn postgres_set_owner_budget_overwrite() {
    let fix = PgFixture::start().await;
    let engine = &fix.engine;

    let j = engine.enqueue_job(&job("overwrite")).await.unwrap();

    engine.set_owner_budget(j.id, 100).await.unwrap();
    engine.set_owner_budget(j.id, 500).await.unwrap();

    // Should have 500, not 100. Exact drain succeeds.
    let outcome = engine.reserve_budget(j.id, 500, "drain").await.unwrap();
    assert_eq!(outcome, ReservationOutcome::Reserved);
}

/// refund_budget on a job with no budget (budget_remaining_cents IS NULL)
/// is a no-op (the WHERE clause excludes it).
#[tokio::test]
async fn postgres_refund_no_budget_noop() {
    let fix = PgFixture::start().await;
    let engine = &fix.engine;

    let j = engine.enqueue_job(&job("no-budget-refund")).await.unwrap();

    // Refunding a job without budget should not error.
    engine.refund_budget(j.id, 100, "pointless").await.unwrap();

    // Still NoBudget.
    let outcome = engine.reserve_budget(j.id, 1, "check").await.unwrap();
    assert_eq!(outcome, ReservationOutcome::NoBudget);
}

/// inherit_budget_owner on a job with no budget IS NULL is a no-op.
#[tokio::test]
async fn postgres_inherit_no_budget_noop() {
    let fix = PgFixture::start().await;
    let engine = &fix.engine;

    let j = engine.enqueue_job(&job("no-budget-inherit")).await.unwrap();
    let other = engine.enqueue_job(&job("other")).await.unwrap();

    // Should not error.
    engine.inherit_budget_owner(j.id, other.id).await.unwrap();

    // Owner should still be None.
    let owner = engine.get_budget_owner(j.id).await.unwrap();
    assert_eq!(owner, None);
}
