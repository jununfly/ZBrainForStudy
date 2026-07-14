//! Contextual-reindex handler — reindex pages with LLM-generated context.
//!
//! ## TS reference
//!
//! `src/commands/jobs.ts` — `contextual_reindex_per_chunk` handler (247 lines).
//! Uses Haiku LLM + rate-lease protection + two-phase build + page-level
//! fallback. v1 skeleton: Haiku LLM + rate-lease not yet ported.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct ContextualReindexHandler;

#[async_trait]
impl MinionHandler for ContextualReindexHandler {
    async fn handle(&self, _ctx: &MinionJobContext) -> Result<Value> {
        // v1: Haiku LLM synopsis + rate-lease not yet ported (grill Q5).
        Ok(json!({"status": "not_implemented", "detail": "contextual-reindex pending Haiku LLM + rate-lease port"}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::BrainEngine;
    use crate::minions::handler::MinionJobContext;
    use crate::InMemoryEngine;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    fn engine() -> Arc<dyn BrainEngine> {
        Arc::new(InMemoryEngine::new())
    }

    #[tokio::test]
    async fn contextual_reindex_smoke_does_not_panic() {
        let eng = engine();
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "contextual_reindex_per_chunk".into(), json!({}), 0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = ContextualReindexHandler;
        let result = handler.handle(&context).await.expect("should not panic");
        assert_eq!(result["status"], "not_implemented");
    }
}
