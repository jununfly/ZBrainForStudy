//! Subagent aggregator handler — collects child subagent results and
//! produces a markdown summary.
//!
//! ## TS reference
//!
//! `src/core/minions/handlers/subagent-aggregator.ts` — reads `child_done`
//! inbox messages, concatenates them deterministically into a markdown report.
//!
//! ## v1 scope
//!
//! Deterministic concatenation only (matches TS v0.15 behaviour). LLM
//! synthesis is deferred to v0.16+.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

/// Aggregates child subagent completions into a markdown summary.
pub struct SubagentAggregatorHandler;

#[async_trait]
impl MinionHandler for SubagentAggregatorHandler {
    async fn handle(&self, ctx: &MinionJobContext) -> Result<Value> {
        let messages = ctx.read_inbox().await?;

        // Concatenate inbox payloads into a markdown report.
        let mut parts: Vec<String> = Vec::new();
        for msg in &messages {
            let body = serde_json::to_string_pretty(&msg.payload).unwrap_or_default();
            let from = &msg.sender;
            parts.push(format!("## Result from {from}\n\n```json\n{body}\n```\n"));
        }

        let summary = if parts.is_empty() {
            "*No subagent results yet.*".to_string()
        } else {
            parts.join("\n")
        };

        Ok(json!({
            "child_count": parts.len(),
            "summary": summary,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::BrainEngine;
    use crate::minions::handler::MinionJobContext;
    use crate::minions::types::MinionJobInput;
    use crate::InMemoryEngine;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    fn engine() -> Arc<dyn BrainEngine> {
        Arc::new(InMemoryEngine::new())
    }

    async fn ctx(engine: &Arc<dyn BrainEngine>, data: Value) -> MinionJobContext {
        // Enqueue + claim the job so we get a valid id + lock_token.
        engine
            .enqueue_job(&MinionJobInput {
                name: "subagent_aggregator".to_string(),
                data: Some(data),
                ..Default::default()
            })
            .await
            .expect("enqueue");

        let claimed = engine
            .claim_job("test", 30_000, "default", &["subagent_aggregator".to_string()])
            .await
            .expect("claim")
            .expect("a job");

        MinionJobContext::new(
            Arc::clone(engine) as Arc<dyn BrainEngine>,
            claimed.id,
            claimed.name,
            claimed.data,
            claimed.attempts_made,
            claimed.lock_token.unwrap_or_default(),
            CancellationToken::new(),
            CancellationToken::new(),
        )
    }

    #[tokio::test]
    async fn subagent_aggregator_returns_no_results_initially() {
        let eng = engine();
        let handler = SubagentAggregatorHandler;
        let context = ctx(&eng, json!({})).await;

        let result = handler.handle(&context).await.expect("handle should succeed");
        assert_eq!(result["child_count"], 0);
        assert!(result["summary"].as_str().unwrap().contains("No subagent results"));
    }
}
