//! Contract test for AdminQueries::list_agent_client_spend.
//!
//! Verifies that the InMemoryEngine stub returns graceful-degradation
//! defaults (empty vec) — matching the behavior when spend/mcp tables
//! do not exist in the Rust schema yet.

use zbrain_core::admin_queries::{AdminQueries, AgentClientSpend};
use zbrain_core::InMemoryEngine;

async fn init_in_memory_admin() -> Box<dyn AdminQueries> {
    let engine = InMemoryEngine::default();
    Box::new(engine)
}

#[tokio::test]
async fn inmemory_spend_returns_empty_vec() {
    let queries = init_in_memory_admin().await;
    let result = queries.list_agent_client_spend().await;
    assert!(result.is_ok(), "list_agent_client_spend should not error");
    let spend = result.unwrap();
    assert!(spend.is_empty(), "InMemory stub must return empty vec (graceful degradation)");
}

#[test]
fn agent_client_spend_serializes_camel_case() {
    let entry = AgentClientSpend {
        client_id: "c1".into(),
        client_name: "Test Agent".into(),
        cap_usd_per_day: Some(10.0),
        spent_cents_today: 500,
        pending_cents: 100,
        inflight_count: 3,
    };
    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(json["clientId"], "c1");
    assert_eq!(json["clientName"], "Test Agent");
    assert_eq!(json["capUsdPerDay"], 10.0);
    assert_eq!(json["spentCentsToday"], 500);
    assert_eq!(json["pendingCents"], 100);
    assert_eq!(json["inflightCount"], 3);
}
