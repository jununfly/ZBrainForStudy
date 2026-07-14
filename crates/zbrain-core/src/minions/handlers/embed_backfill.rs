//! Embed-backfill handler — backfill embeddings for existing pages.
//!
//! ## TS reference
//!
//! `src/commands/jobs.ts` — `embed-backfill` handler (182 lines). Uses
//! embedding API + BudgetTracker. v1 skeleton: BudgetTracker not yet ported.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct EmbedBackfillHandler;

#[async_trait]
impl MinionHandler for EmbedBackfillHandler {
    async fn handle(&self, _ctx: &MinionJobContext) -> Result<Value> {
        // v1: BudgetTracker + embedding backfill pipeline not yet ported.
        Ok(json!({"status": "not_implemented", "detail": "embed-backfill pending BudgetTracker port"}))
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
    async fn embed_backfill_smoke_does_not_panic() {
        let eng = engine();
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "embed-backfill".into(), json!({}), 0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = EmbedBackfillHandler;
        let result = handler.handle(&context).await.expect("should not panic");
        assert_eq!(result["status"], "not_implemented");
    }
}
