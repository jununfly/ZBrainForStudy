//! Integration tests for `recursive_walk` operation (1-6-7-10-5).
//!
//! Tests the InMemory backend BFS recursive code graph traversal:
//! - Basic callers/callees traversal
//! - Cycle detection
//! - Depth cap truncation
//! - Max nodes truncation
//! - NotFound contract
//! - Source scoping
//! - Confidence formula

use zbrain_core::engine::BrainEngine;
use zbrain_core::engine::InMemoryEngine;
use zbrain_core::import::{
    CodeEdgeInput, RecursiveWalkOpts, RecursiveWalkResult, WalkDirection, WalkTruncation,
};

// ─── helpers ────────────────────────────────────────────────────────────────

/// Push a code edge into the engine via the public `add_code_edges` trait method.
/// Each edge gets unique chunk IDs to avoid deduplication.
async fn add_edge(engine: &InMemoryEngine, from: &str, to: &str, source_id: &str) {
    use std::sync::atomic::{AtomicI64, Ordering};
    static CHUNK_SEQ: AtomicI64 = AtomicI64::new(100);
    let from_cid = CHUNK_SEQ.fetch_add(2, Ordering::SeqCst);
    let to_cid = CHUNK_SEQ.fetch_add(2, Ordering::SeqCst);
    let edges = vec![CodeEdgeInput {
        from_chunk_id: from_cid,
        to_chunk_id: Some(to_cid),
        from_symbol_qualified: from.to_string(),
        to_symbol_qualified: to.to_string(),
        edge_type: "calls".to_string(),
        edge_metadata: serde_json::Value::Null,
        source_id: Some(source_id.to_string()),
    }];
    engine.add_code_edges(&edges).await.unwrap();
}

fn make_opts(direction: WalkDirection, source_id: &str) -> RecursiveWalkOpts {
    RecursiveWalkOpts {
        direction,
        depth_cap: None,
        max_nodes: None,
        source_id: source_id.to_string(),
        exact: Some(true),
    }
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_recursive_walk_callers_basic() {
    // Graph: caller_a -> target -> callee_b
    // Callers of "target" should find caller_a
    let engine = InMemoryEngine::default();
    add_edge(&engine, "caller_a", "target", "src1").await;
    add_edge(&engine, "target", "callee_b", "src1").await;

    let opts = make_opts(WalkDirection::Callers, "src1");
    let result = engine.recursive_walk("target", &opts).await.unwrap();

    match result {
        RecursiveWalkResult::Ok {
            depth_groups,
            cycles_detected,
            truncation,
            ..
        } => {
            assert!(!cycles_detected);
            assert_eq!(truncation, WalkTruncation::None);
            // Depth 1 should have caller_a
            assert!(!depth_groups.is_empty());
            let depth1_nodes: Vec<&str> = depth_groups[0]
                .nodes
                .iter()
                .map(|n| n.symbol.as_str())
                .collect();
            assert!(depth1_nodes.contains(&"caller_a"));
            // Confidence should be clamped between 0.05 and 1.0
            for g in &depth_groups {
                assert!(g.confidence >= 0.05);
                assert!(g.confidence <= 1.0);
            }
        }
        other => panic!("expected Ok, got {:?}", other),
    }
}

#[tokio::test]
async fn test_recursive_walk_callees_basic() {
    // Graph: target -> callee_a, target -> callee_b
    // Callees of "target" should find callee_a and callee_b
    let engine = InMemoryEngine::default();
    add_edge(&engine, "target", "callee_a", "src1").await;
    add_edge(&engine, "target", "callee_b", "src1").await;

    let opts = make_opts(WalkDirection::Callees, "src1");
    let result = engine.recursive_walk("target", &opts).await.unwrap();

    match result {
        RecursiveWalkResult::Ok { depth_groups, .. } => {
            assert!(!depth_groups.is_empty());
            let depth1_nodes: Vec<&str> = depth_groups[0]
                .nodes
                .iter()
                .map(|n| n.symbol.as_str())
                .collect();
            assert!(depth1_nodes.contains(&"callee_a"));
            assert!(depth1_nodes.contains(&"callee_b"));
        }
        other => panic!("expected Ok, got {:?}", other),
    }
}

#[tokio::test]
async fn test_recursive_walk_cycle_detection() {
    // Graph: a -> b -> c -> a (cycle)
    let engine = InMemoryEngine::default();
    add_edge(&engine, "a", "b", "src1").await;
    add_edge(&engine, "b", "c", "src1").await;
    add_edge(&engine, "c", "a", "src1").await; // cycle back to a

    let opts = RecursiveWalkOpts {
        direction: WalkDirection::Callees,
        depth_cap: Some(10),
        max_nodes: Some(50),
        source_id: "src1".to_string(),
        exact: Some(true),
    };
    let result = engine.recursive_walk("a", &opts).await.unwrap();

    match result {
        RecursiveWalkResult::Ok {
            cycles_detected, ..
        } => {
            assert!(cycles_detected, "should detect cycle a->b->c->a");
        }
        other => panic!("expected Ok, got {:?}", other),
    }
}

#[tokio::test]
async fn test_recursive_walk_truncation_depth_cap() {
    // Graph: chain1 -> chain2 -> chain3 -> chain4 -> chain5
    let engine = InMemoryEngine::default();
    add_edge(&engine, "chain1", "chain2", "src1").await;
    add_edge(&engine, "chain2", "chain3", "src1").await;
    add_edge(&engine, "chain3", "chain4", "src1").await;
    add_edge(&engine, "chain4", "chain5", "src1").await;

    let opts = RecursiveWalkOpts {
        direction: WalkDirection::Callees,
        depth_cap: Some(2),
        max_nodes: Some(100),
        source_id: "src1".to_string(),
        exact: Some(true),
    };
    let result = engine.recursive_walk("chain1", &opts).await.unwrap();

    match result {
        RecursiveWalkResult::Ok {
            depth_groups,
            truncation,
            ..
        } => {
            assert_eq!(truncation, WalkTruncation::DepthCap);
            // Should only have 2 depth groups
            assert_eq!(depth_groups.len(), 2);
        }
        other => panic!("expected Ok, got {:?}", other),
    }
}

#[tokio::test]
async fn test_recursive_walk_truncation_max_nodes() {
    // Bushy graph: root -> n1, n2, n3, n4, n5
    let engine = InMemoryEngine::default();
    add_edge(&engine, "root", "n1", "src1").await;
    add_edge(&engine, "root", "n2", "src1").await;
    add_edge(&engine, "root", "n3", "src1").await;
    add_edge(&engine, "root", "n4", "src1").await;
    add_edge(&engine, "root", "n5", "src1").await;

    let opts = RecursiveWalkOpts {
        direction: WalkDirection::Callees,
        depth_cap: Some(10),
        max_nodes: Some(3),
        source_id: "src1".to_string(),
        exact: Some(true),
    };
    let result = engine.recursive_walk("root", &opts).await.unwrap();

    match result {
        RecursiveWalkResult::Ok {
            depth_groups,
            truncation,
            ..
        } => {
            assert_eq!(truncation, WalkTruncation::MaxNodes);
            // Total nodes across all depth groups should be <= 3
            let total: usize = depth_groups.iter().map(|g| g.nodes.len()).sum();
            assert!(total <= 3);
        }
        other => panic!("expected Ok, got {:?}", other),
    }
}

#[tokio::test]
async fn test_recursive_walk_not_found() {
    let engine = InMemoryEngine::default();
    add_edge(&engine, "a", "b", "src1").await;

    // Non-exact mode: disambiguate_symbol won't find "nonexistent"
    let opts = RecursiveWalkOpts {
        direction: WalkDirection::Callers,
        depth_cap: None,
        max_nodes: None,
        source_id: "src1".to_string(),
        exact: Some(false),
    };
    let result = engine.recursive_walk("nonexistent", &opts).await.unwrap();

    match result {
        RecursiveWalkResult::NotFound { did_you_mean } => {
            // Should have empty or near-empty did_you_mean
            assert!(did_you_mean.is_empty() || did_you_mean.len() <= 10);
        }
        other => panic!("expected NotFound, got {:?}", other),
    }
}

#[tokio::test]
async fn test_recursive_walk_source_scoping() {
    // Same symbol in two different sources
    let engine = InMemoryEngine::default();
    add_edge(&engine, "shared", "only_in_src1", "src1").await;
    add_edge(&engine, "shared", "only_in_src2", "src2").await;

    // Query with source_id = "src2" should only see edges from src2
    let opts = make_opts(WalkDirection::Callees, "src2");
    let result = engine.recursive_walk("shared", &opts).await.unwrap();

    match result {
        RecursiveWalkResult::Ok { depth_groups, .. } => {
            if !depth_groups.is_empty() {
                for n in &depth_groups[0].nodes {
                    assert_eq!(
                        n.symbol, "only_in_src2",
                        "node {} should be from src2 only",
                        n.symbol
                    );
                }
            }
        }
        other => panic!("expected Ok, got {:?}", other),
    }
}

#[tokio::test]
async fn test_recursive_walk_exact_mode_skips_disambiguation() {
    // With exact=true, the symbol is used as-is without disambiguation
    let engine = InMemoryEngine::default();
    add_edge(&engine, "exact_start", "callee1", "src1").await;

    let opts = RecursiveWalkOpts {
        direction: WalkDirection::Callees,
        depth_cap: None,
        max_nodes: None,
        source_id: "src1".to_string(),
        exact: Some(true),
    };
    let result = engine.recursive_walk("exact_start", &opts).await.unwrap();

    match result {
        RecursiveWalkResult::Ok { depth_groups, .. } => {
            assert!(!depth_groups.is_empty());
            assert_eq!(depth_groups[0].nodes[0].symbol, "callee1");
        }
        other => panic!("expected Ok, got {:?}", other),
    }
}

#[tokio::test]
async fn test_recursive_walk_confidence_formula() {
    // Verify confidence = 1/(1+0.3*depth), clamped to [0.05, 1.0]
    let engine = InMemoryEngine::default();
    add_edge(&engine, "d0", "d1", "src1").await;
    add_edge(&engine, "d1", "d2", "src1").await;
    add_edge(&engine, "d2", "d3", "src1").await;

    let opts = RecursiveWalkOpts {
        direction: WalkDirection::Callees,
        depth_cap: Some(5),
        max_nodes: Some(100),
        source_id: "src1".to_string(),
        exact: Some(true),
    };
    let result = engine.recursive_walk("d0", &opts).await.unwrap();

    match result {
        RecursiveWalkResult::Ok { depth_groups, .. } => {
            for g in &depth_groups {
                let expected = 1.0 / (1.0 + 0.3 * g.depth as f32);
                let clamped = expected.max(0.05).min(1.0);
                assert!(
                    (g.confidence - clamped).abs() < 0.001,
                    "depth {} confidence {} != expected {}",
                    g.depth,
                    g.confidence,
                    clamped
                );
            }
        }
        other => panic!("expected Ok, got {:?}", other),
    }
}
