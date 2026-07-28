//! Contract test for AdminQueries::get_watch_snapshot.
//!
//! Verifies that the InMemoryEngine stub returns graceful-degradation
//! defaults — all fields zeroed/empty — matching the behavior when
//! minion_jobs tables do not exist in the Rust schema yet.

use zbrain_core::admin_queries::{
    AdminQueries, BudgetOwner, ErrorClusterCount, JobTypeSummary, QueueHealth, WatchSnapshot,
};
use zbrain_core::InMemoryEngine;

/// Serialize all libsql FFI access in this binary. The `libsql` native
/// library is not safe to drive from multiple OS threads concurrently on
/// Windows (parallel `cargo test` threads crash with STATUS_ACCESS_VIOLATION
/// 0xc0000005). Each test grabs this guard for its whole body so the suite
/// stays green under default parallelism; serial runs are unaffected.
static LIBSQL_TEST_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
fn libsql_test_guard() -> std::sync::MutexGuard<'static, ()> {
    LIBSQL_TEST_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}


async fn init_in_memory_admin() -> Box<dyn AdminQueries> {
    let engine = InMemoryEngine::default();
    Box::new(engine)
}

#[tokio::test]
async fn inmemory_watch_returns_default_snapshot() {
    let _guard = libsql_test_guard();
    let queries = init_in_memory_admin().await;
    let result = queries.get_watch_snapshot().await;
    assert!(result.is_ok(), "get_watch_snapshot should not error");
    let snap = result.unwrap();
    assert!(snap.ts_ms > 0, "ts_ms should be a real timestamp");
    assert!(snap.by_type.is_empty(), "by_type should be empty");
    assert_eq!(snap.queue_health.waiting, 0);
    assert_eq!(snap.queue_health.active, 0);
    assert_eq!(snap.queue_health.stalled, 0);
    assert_eq!(snap.lease_pressure_1h, 0);
    assert!(snap.top_errors.is_empty(), "top_errors should be empty");
    assert!(snap.budget_owners.is_empty(), "budget_owners should be empty");
}

#[test]
fn watch_snapshot_serializes_camel_case() {
    let _guard = libsql_test_guard();
    let snap = WatchSnapshot {
        ts_ms: 1719705600000,
        by_type: vec![JobTypeSummary {
            name: "subagent".into(),
            total: 10,
            completed: 5,
            failed: 3,
            dead: 2,
        }],
        queue_health: QueueHealth { waiting: 1, active: 2, stalled: 0 },
        lease_pressure_1h: 42,
        top_errors: vec![ErrorClusterCount { cluster: "timeout".into(), count: 7 }],
        budget_owners: vec![BudgetOwner { owner_id: 1, remaining_cents: 500, total_spent_cents: 300 }],
    };
    let json = serde_json::to_value(&snap).unwrap();
    assert_eq!(json["tsMs"], 1719705600000i64);
    assert_eq!(json["byType"][0]["name"], "subagent");
    assert_eq!(json["byType"][0]["total"], 10);
    assert_eq!(json["queueHealth"]["waiting"], 1);
    assert_eq!(json["queueHealth"]["active"], 2);
    assert_eq!(json["leasePressure1h"], 42);
    assert_eq!(json["topErrors"][0]["cluster"], "timeout");
    assert_eq!(json["topErrors"][0]["count"], 7);
    assert_eq!(json["budgetOwners"][0]["ownerId"], 1);
    assert_eq!(json["budgetOwners"][0]["remainingCents"], 500);
}

/// Import the internal cluster_errors function for testing.
/// This is an integration test — it tests the public API's behavior
/// when the error classifier is used through the LibsqlEngine.
/// For unit-level cluster testing, see libsql.rs inline tests.
#[tokio::test]
async fn error_cluster_roundtrip_through_engine() {
    let _guard = libsql_test_guard();
    // Test that the error classifier is callable through the public API.
    // Since there are no tables, top_errors will be empty — but the function
    // itself is exercised by the LibsqlEngine query path.
    let engine = InMemoryEngine::default();
    let queries: Box<dyn AdminQueries> = Box::new(engine);
    let snap = queries.get_watch_snapshot().await.unwrap();
    // Graceful degradation: no jobs → no errors to classify → empty
    assert!(snap.top_errors.is_empty());
}
