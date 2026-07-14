//! Recompute-emotional-weight handler — recompute emotional weight phase.
//!
//! ## TS reference
//!
//! Registered as `"recompute_emotional_weight"` in `src/commands/jobs.ts`.
//! Core logic in `src/core/autopilot/cycle.ts` recompute_emotional_weight
//! phase. v1 skeleton: runCycle not yet ported.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct RecomputeEmotionalWeightHandler;

#[async_trait]
impl MinionHandler for RecomputeEmotionalWeightHandler {
    async fn handle(&self, _ctx: &MinionJobContext) -> Result<Value> {
        Ok(json!({"status": "not_implemented", "detail": "recompute_emotional_weight phase pending runCycle port"}))
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
    async fn recompute_emotional_weight_smoke_does_not_panic() {
        let eng = engine();
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "recompute_emotional_weight".into(), json!({}), 0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = RecomputeEmotionalWeightHandler;
        let result = handler.handle(&context).await.expect("should not panic");
        assert_eq!(result["status"], "not_implemented");
    }
}
