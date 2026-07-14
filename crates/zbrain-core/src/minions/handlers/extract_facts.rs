//! Extract-facts handler — extract facts phase of the autopilot cycle.
//!
//! ## TS reference
//!
//! Registered as `"extract_facts"` in `src/commands/jobs.ts`. Core logic in
//! `src/core/autopilot/cycle.ts` extract_facts phase. v1 skeleton: runCycle
//! not yet ported.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct ExtractFactsHandler;

#[async_trait]
impl MinionHandler for ExtractFactsHandler {
    async fn handle(&self, _ctx: &MinionJobContext) -> Result<Value> {
        Ok(json!({"status": "not_implemented", "detail": "extract_facts phase pending runCycle port"}))
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
    async fn extract_facts_smoke_does_not_panic() {
        let eng = engine();
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "extract_facts".into(), json!({}), 0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = ExtractFactsHandler;
        let result = handler.handle(&context).await.expect("should not panic");
        assert_eq!(result["status"], "not_implemented");
    }
}
